//! Use case: block until any of N substring patterns appears in the shell
//! output buffer, the shell closes, or a deadline expires.
//!
//! Translates the v3 `ssh_shell_wait_for_impl` (from
//! `src/mcp/tools/shell.rs`) into a hexagonal use case orchestrating ports.
//! No rmcp / russh / tokio types leak into the public DTO surface.
//!
//! # Orchestration shape
//!
//! 1. Validate the request: `patterns` must contain 1..=16 entries
//!    ([`DomainError::InvalidArgument`] with code `EMPTY_PATTERNS` /
//!    `TOO_MANY_PATTERNS`), each entry must be ≤1024 bytes
//!    ([`DomainError::InvalidArgument`] with code `PATTERN_TOO_LONG`).
//! 2. Verify the shell exists via [`ShellRepository::get`]. A missing id
//!    short-circuits with [`DomainError::ShellNotFound`].
//! 3. Compute the deadline as `now + min(timeout, 300s)`. The default is
//!    30s when [`WaitForPatternRequest::timeout`] is `None`.
//! 4. Loop:
//!    - Snapshot the shell's rolling buffer through
//!      [`OutputStreamPort::snapshot_shell`].
//!    - Scan the unscanned tail (rewinding by `max_pat_len - 1` to catch
//!      matches straddling chunks) for the earliest occurrence of any
//!      pattern. Ties broken by pattern order (first wins).
//!    - On match: return [`WaitForPatternStatus::Matched`] with the
//!      pattern + buffer window.
//!    - When the snapshot reports the shell is closed (the rolling buffer
//!      stops growing and the underlying repository flips the status to
//!      [`crate::domain::shell::ShellStatus::Closed`]): return
//!      [`WaitForPatternStatus::Closed`] with the current buffer.
//!    - When the deadline elapses: return
//!      [`WaitForPatternStatus::Timeout`] with the current buffer.
//!    - Otherwise sleep [`POLL_INTERVAL`] (50 ms) and retry.
//!
//! # Concurrency contract
//!
//! - Every `.await` happens on owned values returned by ports — no
//!   repository / fake guard is held across an await.
//! - The use case clones small handles (`Arc`) into local bindings before
//!   entering await points.
//! - The poll cadence is bounded so a never-matching pattern combined with
//!   a healthy shell does not spin: each iteration costs at most one
//!   port call plus the 50 ms sleep.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::time::sleep;

use crate::domain::error::DomainError;
use crate::domain::ids::ShellId;
use crate::domain::shell::ShellStatus;
use crate::ports::clock::ClockPort;
use crate::ports::config::ConfigPort;
use crate::ports::output_stream::OutputStreamPort;
use crate::ports::shell_repo::ShellRepository;

/// Maximum number of patterns accepted in a single request.
const MAX_PATTERNS: usize = 16;

/// Maximum length of any single pattern in bytes.
const MAX_PATTERN_BYTES: usize = 1024;

/// Default wait timeout when [`WaitForPatternRequest::timeout`] is `None`.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Hard cap on the wait timeout (5 minutes).
const MAX_TIMEOUT: Duration = Duration::from_secs(300);

/// Cooperative polling cadence between snapshot scans. Small enough to keep
/// matched-prompt latency under one human tick while avoiding a tight spin.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Inbound DTO. Built by the rmcp tool wrapper (etapa H16).
#[derive(Debug, Clone)]
pub struct WaitForPatternRequest {
    /// `SHELL_ID` returned from `ssh_shell_open`.
    pub shell_id: ShellId,
    /// 1..=16 substring patterns. The first match wins (earliest position
    /// in the buffer; ties broken by the order of this slice). Each pattern
    /// must be ≤1024 bytes.
    pub patterns: Vec<String>,
    /// Maximum time to block. `None` defers to [`DEFAULT_TIMEOUT`]; capped
    /// at [`MAX_TIMEOUT`].
    pub timeout: Option<Duration>,
    /// Optional cap on bytes returned in [`WaitForPatternOutcome::data`].
    /// `None` returns the full snapshot (the inbound adapter is
    /// responsible for clamping in the v3 schema, but the use case honours
    /// the field when supplied so callers can shave allocation cost).
    /// Truncation preserves the *tail* (most recent bytes), matching v3
    /// `render_output_block` semantics.
    pub max_output_bytes: Option<usize>,
    /// Whether the matched prefix should be drained from the rolling
    /// buffer after returning. The use case does not perform the drain
    /// itself — the rolling buffer lives in the shell aggregate, not in
    /// this use case's port surface — but the field is preserved on the
    /// DTO so the inbound adapter can pass it through to a future
    /// `drain_shell` port (recorded here for forward compatibility).
    pub clear: bool,
}

/// Outbound DTO surfacing every observable result the rmcp tool wrapper
/// must render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitForPatternOutcome {
    /// Echoed back so the caller can correlate without recreating the request.
    pub shell_id: ShellId,
    /// Final status reached by the wait loop.
    pub status: WaitForPatternStatus,
    /// The pattern that matched, when [`Self::status`] is
    /// [`WaitForPatternStatus::Matched`]. `None` for `Timeout`/`Closed`.
    pub matched_pattern: Option<String>,
    /// Buffer window observed at return time. Truncated according to
    /// [`WaitForPatternRequest::max_output_bytes`] (tail-preserving).
    pub data: Bytes,
    /// Latest sequence number from the output stream snapshot. Lets clients
    /// correlate this snapshot with subsequent `resources/read` cursor pages.
    pub last_seq: u64,
}

/// Final status reached by [`WaitForPatternUseCase::execute`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitForPatternStatus {
    /// One of the patterns matched.
    Matched,
    /// The deadline expired before any pattern matched.
    Timeout,
    /// The shell transitioned to [`ShellStatus::Closed`] before any
    /// pattern matched.
    Closed,
}

/// Wait-for-pattern use case generic over every adapter dependency.
#[derive(Debug)]
pub struct WaitForPatternUseCase<ShR, OS, C, Cfg>
where
    ShR: ShellRepository + Send + Sync,
    OS: OutputStreamPort + Send + Sync,
    C: ClockPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
{
    shells: Arc<ShR>,
    streams: Arc<OS>,
    /// Held so the composition root can wire the same clock handle every
    /// other use case receives. The wait loop itself uses
    /// [`std::time::Instant::now`] for the deadline because the loop sleeps
    /// on a real `tokio::time` timer (not a virtual clock); using the
    /// real monotonic clock keeps the deadline check consistent with the
    /// sleep cadence and matches the v3 `wait_for_pattern_loop` semantics.
    #[allow(
        dead_code,
        reason = "wired for forward-compatible clock-driven knobs (e.g. test-virtualised loop)"
    )]
    clock: Arc<C>,
    /// Held for parity with the v3 `wait_for` tool surface; future work
    /// (e.g. configurable poll cadence) will read it.
    #[allow(
        dead_code,
        reason = "wired for forward-compatible config-driven knobs (poll cadence, broadcast cap)"
    )]
    config: Arc<Cfg>,
}

impl<ShR, OS, C, Cfg> WaitForPatternUseCase<ShR, OS, C, Cfg>
where
    ShR: ShellRepository + Send + Sync,
    OS: OutputStreamPort + Send + Sync,
    C: ClockPort + Send + Sync,
    Cfg: ConfigPort + Send + Sync,
{
    /// Wire the use case from already-shared adapter handles.
    #[must_use]
    pub const fn new(shells: Arc<ShR>, streams: Arc<OS>, clock: Arc<C>, config: Arc<Cfg>) -> Self {
        Self {
            shells,
            streams,
            clock,
            config,
        }
    }

    /// Drive the wait orchestration. See module-level docs for the
    /// step-by-step semantics.
    ///
    /// # Errors
    ///
    /// - [`DomainError::InvalidArgument`] when `patterns` is empty
    ///   (`EMPTY_PATTERNS`), exceeds [`MAX_PATTERNS`] (`TOO_MANY_PATTERNS`),
    ///   or contains a pattern longer than [`MAX_PATTERN_BYTES`]
    ///   (`PATTERN_TOO_LONG`).
    /// - [`DomainError::ShellNotFound`] when the id is unknown at probe time.
    /// - Any [`DomainError`] surfaced by the underlying ports.
    pub async fn execute(
        &self,
        req: WaitForPatternRequest,
    ) -> Result<WaitForPatternOutcome, DomainError> {
        validate_patterns(&req.patterns)?;
        self.ensure_shell_exists(&req.shell_id).await?;

        let timeout = req.timeout.unwrap_or(DEFAULT_TIMEOUT).min(MAX_TIMEOUT);
        let deadline = Instant::now() + timeout;
        let max_pat_len = req
            .patterns
            .iter()
            .map(String::len)
            .max()
            .unwrap_or(0_usize);

        self.poll_loop(&req, deadline, max_pat_len).await
    }

    /// Verify the shell exists in the repository. A missing id maps to
    /// [`DomainError::ShellNotFound`] so the inbound adapter can render the
    /// canonical `SHELL_NOT_FOUND` error block.
    async fn ensure_shell_exists(&self, shell_id: &ShellId) -> Result<(), DomainError> {
        let entity = self.shells.get(shell_id).await?;
        if entity.is_none() {
            return Err(DomainError::ShellNotFound(shell_id.clone()));
        }
        Ok(())
    }

    /// Cooperative polling loop.
    ///
    /// Each iteration:
    /// 1. Take an [`crate::ports::output_stream::OutputSnapshot`].
    /// 2. Scan the unscanned tail; rewind by `max_pat_len - 1` so a match
    ///    that straddles two scans is still detected.
    /// 3. Re-check the shell's lifecycle: a closed shell terminates the
    ///    loop with [`WaitForPatternStatus::Closed`].
    /// 4. Re-check the deadline; expiry terminates the loop with
    ///    [`WaitForPatternStatus::Timeout`].
    /// 5. Sleep [`POLL_INTERVAL`] before the next iteration.
    async fn poll_loop(
        &self,
        req: &WaitForPatternRequest,
        deadline: Instant,
        max_pat_len: usize,
    ) -> Result<WaitForPatternOutcome, DomainError> {
        let mut last_scanned_offset = 0_usize;
        loop {
            if let Some(out) = self
                .poll_once(req, deadline, max_pat_len, &mut last_scanned_offset)
                .await?
            {
                return Ok(out);
            }
            sleep(POLL_INTERVAL).await;
        }
    }

    /// Run a single iteration of [`Self::poll_loop`]. Returns
    /// `Ok(Some(outcome))` when a terminal state was reached this tick
    /// (matched, closed, or timed out) and `Ok(None)` when the caller
    /// should sleep and try again.
    async fn poll_once(
        &self,
        req: &WaitForPatternRequest,
        deadline: Instant,
        max_pat_len: usize,
        last_scanned_offset: &mut usize,
    ) -> Result<Option<WaitForPatternOutcome>, DomainError> {
        let snapshot = self.streams.snapshot_shell(&req.shell_id).await?;
        let scan_start = last_scanned_offset.saturating_sub(max_pat_len.saturating_sub(1_usize));
        if let Some(matched) = scan_for_first_match(&snapshot.stdout, &req.patterns, scan_start) {
            return Ok(Some(build_matched(
                req,
                &snapshot.stdout,
                snapshot.last_seq,
                matched,
            )));
        }
        *last_scanned_offset = snapshot.stdout.len();
        let terminal = self.terminal_status(&req.shell_id, deadline).await?;
        Ok(terminal.map(|status| build_terminal(req, snapshot.stdout, snapshot.last_seq, status)))
    }

    /// Decide whether the loop should terminate this tick because the
    /// shell flipped to closed or the deadline expired. Returns
    /// `Ok(None)` when neither condition holds.
    async fn terminal_status(
        &self,
        shell_id: &ShellId,
        deadline: Instant,
    ) -> Result<Option<WaitForPatternStatus>, DomainError> {
        if self.is_shell_closed(shell_id).await? {
            return Ok(Some(WaitForPatternStatus::Closed));
        }
        if Instant::now() >= deadline {
            return Ok(Some(WaitForPatternStatus::Timeout));
        }
        Ok(None)
    }

    /// Re-read the shell entity to surface the latest lifecycle state.
    /// Returns `true` when the shell flipped to
    /// [`ShellStatus::Closed`]; absence of the entity is also treated as
    /// closed so the loop never spins on a removed shell.
    async fn is_shell_closed(&self, shell_id: &ShellId) -> Result<bool, DomainError> {
        let entity = self.shells.get(shell_id).await?;
        match entity {
            Some(e) => Ok(e.status == ShellStatus::Closed),
            None => Ok(true),
        }
    }
}

/// Resolved match within the buffer.
struct PatternHit {
    /// Pattern (owned) that matched. Owned because the request DTO may go
    /// out of scope before the inbound adapter renders the response.
    pattern: String,
}

/// Scan `buffer[scan_start..]` for the earliest occurrence of any pattern.
/// Ties broken by the order of the `patterns` slice (first wins).
fn scan_for_first_match(
    buffer: &[u8],
    patterns: &[String],
    scan_start: usize,
) -> Option<PatternHit> {
    if scan_start >= buffer.len() {
        return None;
    }
    let window = &buffer[scan_start..];
    let mut best: Option<(usize, &str)> = None;
    for pattern in patterns {
        let needle = pattern.as_bytes();
        if needle.is_empty() {
            continue;
        }
        let Some(pos) = find_subslice(window, needle) else {
            continue;
        };
        match best {
            Some((cur, _)) if cur <= pos => {}
            Some(_) | None => best = Some((pos, pattern.as_str())),
        }
    }
    best.map(|(_, pattern)| PatternHit {
        pattern: pattern.to_string(),
    })
}

/// Find the first occurrence of `needle` in `haystack`.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Validate the `patterns` slice. The inbound adapter renders a separate
/// error code per failure, so the [`DomainError::InvalidArgument`] payload
/// is prefixed with the v3 code (`EMPTY_PATTERNS`, `TOO_MANY_PATTERNS`,
/// `PATTERN_TOO_LONG`) followed by `:` and a human-readable message.
fn validate_patterns(patterns: &[String]) -> Result<(), DomainError> {
    if patterns.is_empty() {
        return Err(DomainError::InvalidArgument(
            "EMPTY_PATTERNS: patterns must contain at least one entry".to_string(),
        ));
    }
    if patterns.len() > MAX_PATTERNS {
        return Err(DomainError::InvalidArgument(format!(
            "TOO_MANY_PATTERNS: patterns exceeds maximum of {MAX_PATTERNS} (count={})",
            patterns.len()
        )));
    }
    for pattern in patterns {
        if pattern.len() > MAX_PATTERN_BYTES {
            return Err(DomainError::InvalidArgument(format!(
                "PATTERN_TOO_LONG: individual pattern exceeds {MAX_PATTERN_BYTES} bytes (len={})",
                pattern.len()
            )));
        }
    }
    Ok(())
}

/// Build the `Matched` outcome with the appropriate buffer window.
fn build_matched(
    req: &WaitForPatternRequest,
    buffer: &Bytes,
    last_seq: u64,
    matched: PatternHit,
) -> WaitForPatternOutcome {
    let data = head_truncate(buffer.clone(), req.max_output_bytes);
    WaitForPatternOutcome {
        shell_id: req.shell_id.clone(),
        status: WaitForPatternStatus::Matched,
        matched_pattern: Some(matched.pattern),
        data,
        last_seq,
    }
}

/// Build a terminal (`Timeout` or `Closed`) outcome.
fn build_terminal(
    req: &WaitForPatternRequest,
    buffer: Bytes,
    last_seq: u64,
    status: WaitForPatternStatus,
) -> WaitForPatternOutcome {
    let data = head_truncate(buffer, req.max_output_bytes);
    WaitForPatternOutcome {
        shell_id: req.shell_id.clone(),
        status,
        matched_pattern: None,
        data,
        last_seq,
    }
}

/// Head-truncate `buf` so the returned `Bytes` is at most `max` bytes long,
/// preserving the *tail* (most recent output). When `max` is `None` the
/// buffer is returned unchanged.
fn head_truncate(buf: Bytes, max: Option<usize>) -> Bytes {
    let Some(cap) = max else {
        return buf;
    };
    if buf.len() <= cap {
        return buf;
    }
    let drop_head = buf.len() - cap;
    buf.slice(drop_head..)
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_PATTERN_BYTES, WaitForPatternOutcome, WaitForPatternRequest, WaitForPatternStatus,
        WaitForPatternUseCase, find_subslice, head_truncate, scan_for_first_match,
        validate_patterns,
    };
    use crate::adapters::clock::fake::FakeClock;
    use crate::adapters::config::memory::MapConfig;
    use crate::adapters::repo::dashmap::shell::DashMapShellRepo;
    use crate::domain::error::DomainError;
    use crate::domain::ids::{SessionId, ShellId};
    use crate::domain::shell::{ShellEntity, ShellTerminal};
    use crate::ports::output_stream::{OutputSnapshot, OutputStreamPort};
    use crate::ports::shell_repo::ShellRepository;
    use bytes::Bytes;
    use chrono::Utc;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// In-memory [`OutputStreamPort`] fake. Stores per-shell snapshots in a
    /// `Mutex<HashMap>` so the producer thread (a test helper) can mutate
    /// the buffer between consumer polls.
    #[derive(Debug, Default, Clone)]
    struct FakeOutputStream {
        shell_snapshots: Arc<Mutex<std::collections::HashMap<ShellId, OutputSnapshot>>>,
    }

    impl FakeOutputStream {
        fn new() -> Self {
            Self::default()
        }

        fn put_shell(&self, id: &ShellId, snap: OutputSnapshot) {
            let mut map = self.shell_snapshots.lock().expect("snapshot map mutex");
            map.insert(id.clone(), snap);
        }
    }

    impl OutputStreamPort for FakeOutputStream {
        async fn snapshot_command(
            &self,
            id: &crate::domain::ids::CommandId,
        ) -> Result<OutputSnapshot, DomainError> {
            Err(DomainError::CommandNotFound(id.clone()))
        }

        async fn snapshot_shell(&self, id: &ShellId) -> Result<OutputSnapshot, DomainError> {
            let map = self.shell_snapshots.lock().expect("snapshot map mutex");
            map.get(id)
                .cloned()
                .ok_or_else(|| DomainError::ShellNotFound(id.clone()))
        }
    }

    type UseCaseUnderTest =
        WaitForPatternUseCase<DashMapShellRepo, FakeOutputStream, FakeClock, MapConfig>;

    fn build_use_case() -> (
        UseCaseUnderTest,
        Arc<DashMapShellRepo>,
        Arc<FakeOutputStream>,
        Arc<FakeClock>,
    ) {
        let shells = Arc::new(DashMapShellRepo::new());
        let streams = Arc::new(FakeOutputStream::new());
        let clock = Arc::new(FakeClock::new(1_777_982_400_000_u64));
        let config = Arc::new(MapConfig::default_v3());
        let uc = WaitForPatternUseCase::new(
            Arc::clone(&shells),
            Arc::clone(&streams),
            Arc::clone(&clock),
            Arc::clone(&config),
        );
        (uc, shells, streams, clock)
    }

    fn shell_entity(id: &str) -> ShellEntity {
        ShellEntity::new(
            ShellId::new(id.to_string()),
            SessionId::new("s-1".to_string()),
            ShellTerminal::new("xterm-256color".to_string(), 80, 24),
            Utc::now(),
            Duration::from_secs(600),
            10 * 1024 * 1024,
        )
    }

    fn snapshot(stdout: &[u8], last_seq: u64) -> OutputSnapshot {
        OutputSnapshot {
            byte_cursor: 0,
            last_seq,
            stdout: Bytes::copy_from_slice(stdout),
            stderr: Bytes::new(),
        }
    }

    fn base_request(id: &ShellId, patterns: Vec<String>) -> WaitForPatternRequest {
        WaitForPatternRequest {
            shell_id: id.clone(),
            patterns,
            timeout: Some(Duration::from_secs(2)),
            max_output_bytes: None,
            clear: true,
        }
    }

    async fn seed_shell(repo: &DashMapShellRepo, id: &str) -> ShellEntity {
        let entity = shell_entity(id);
        repo.insert(entity.clone())
            .await
            .expect("seed shell insert");
        entity
    }

    fn unwrap_outcome(r: Result<WaitForPatternOutcome, DomainError>) -> WaitForPatternOutcome {
        match r {
            Ok(v) => v,
            Err(e) => panic!("unexpected DomainError: {e:?}"),
        }
    }

    // --- validate_patterns -----------------------------------------------

    #[test]
    fn validate_rejects_empty_patterns() {
        let err = validate_patterns(&[]).expect_err("must reject empty");
        match err {
            DomainError::InvalidArgument(msg) => {
                assert!(msg.starts_with("EMPTY_PATTERNS"), "got {msg}");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_more_than_16_patterns() {
        let patterns: Vec<String> = (0_u8..17_u8).map(|i| format!("p{i}")).collect();
        let err = validate_patterns(&patterns).expect_err("must reject 17");
        match err {
            DomainError::InvalidArgument(msg) => {
                assert!(msg.starts_with("TOO_MANY_PATTERNS"), "got {msg}");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_pattern_longer_than_1024_bytes() {
        let pattern = "x".repeat(MAX_PATTERN_BYTES + 1);
        let err = validate_patterns(&[pattern]).expect_err("must reject");
        match err {
            DomainError::InvalidArgument(msg) => {
                assert!(msg.starts_with("PATTERN_TOO_LONG"), "got {msg}");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn validate_accepts_pattern_at_exactly_1024_bytes() {
        let pattern = "x".repeat(MAX_PATTERN_BYTES);
        validate_patterns(&[pattern]).expect("must accept exactly 1024");
    }

    // --- find_subslice + scan_for_first_match ----------------------------

    #[test]
    fn find_subslice_locates_needle() {
        assert_eq!(find_subslice(b"hello world", b"world"), Some(6));
        assert_eq!(find_subslice(b"hello world", b"xyz"), None);
        assert_eq!(find_subslice(b"abc", b""), None);
        assert_eq!(find_subslice(b"ab", b"abc"), None);
    }

    #[test]
    fn scan_returns_none_when_no_pattern_matches() {
        let patterns = vec!["foo".to_string(), "bar".to_string()];
        assert!(scan_for_first_match(b"baz qux", &patterns, 0).is_none());
    }

    #[test]
    fn scan_picks_earliest_position_across_patterns() {
        // "world" begins at 6, "hello" begins at 0; earliest must win.
        let patterns = vec!["world".to_string(), "hello".to_string()];
        let hit = scan_for_first_match(b"hello world", &patterns, 0).expect("must match");
        assert_eq!(hit.pattern, "hello");
    }

    #[test]
    fn scan_breaks_ties_by_pattern_order() {
        // Both patterns start at the same position 0; "alpha" wins because
        // it precedes "alphabet" in the slice (and "alpha" is a prefix of
        // "alphabet" so both find pos=0).
        let patterns = vec!["alpha".to_string(), "alphabet".to_string()];
        let hit = scan_for_first_match(b"alphabet rocks", &patterns, 0).expect("must match");
        assert_eq!(hit.pattern, "alpha");
    }

    #[test]
    fn scan_skips_empty_patterns() {
        let patterns = vec![String::new(), "ok".to_string()];
        let hit = scan_for_first_match(b"is ok now", &patterns, 0).expect("must match");
        assert_eq!(hit.pattern, "ok");
    }

    #[test]
    fn scan_returns_none_when_scan_start_past_buffer_end() {
        let patterns = vec!["x".to_string()];
        assert!(scan_for_first_match(b"abc", &patterns, 10).is_none());
    }

    // --- head_truncate ---------------------------------------------------

    #[test]
    fn head_truncate_keeps_buffer_when_no_cap() {
        let buf = Bytes::from_static(b"hello");
        assert_eq!(head_truncate(buf.clone(), None), buf);
    }

    #[test]
    fn head_truncate_drops_head_when_over_cap() {
        let buf = Bytes::from_static(b"abcdefghij");
        let out = head_truncate(buf, Some(4));
        assert_eq!(out.as_ref(), b"ghij");
    }

    // --- execute: validation surfaces InvalidArgument ---------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_patterns_returns_invalid_argument_empty_patterns() {
        let (uc, _shells, _streams, _clock) = build_use_case();
        let req = base_request(&ShellId::new("sh-1".to_string()), Vec::new());
        let err = uc.execute(req).await.expect_err("must reject");
        match err {
            DomainError::InvalidArgument(msg) => {
                assert!(msg.starts_with("EMPTY_PATTERNS"), "got {msg}");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn seventeen_patterns_returns_too_many_patterns() {
        let (uc, _shells, _streams, _clock) = build_use_case();
        let patterns: Vec<String> = (0_u8..17_u8).map(|i| format!("p{i}")).collect();
        let req = base_request(&ShellId::new("sh-1".to_string()), patterns);
        let err = uc.execute(req).await.expect_err("must reject");
        match err {
            DomainError::InvalidArgument(msg) => {
                assert!(msg.starts_with("TOO_MANY_PATTERNS"), "got {msg}");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pattern_of_1025_bytes_returns_pattern_too_long() {
        let (uc, _shells, _streams, _clock) = build_use_case();
        let pattern = "x".repeat(MAX_PATTERN_BYTES + 1);
        let req = base_request(&ShellId::new("sh-1".to_string()), vec![pattern]);
        let err = uc.execute(req).await.expect_err("must reject");
        match err {
            DomainError::InvalidArgument(msg) => {
                assert!(msg.starts_with("PATTERN_TOO_LONG"), "got {msg}");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    // --- execute: shell-not-found ----------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_shell_id_returns_shell_not_found() {
        let (uc, _shells, _streams, _clock) = build_use_case();
        let req = base_request(&ShellId::new("absent".to_string()), vec!["ok".to_string()]);
        let err = uc.execute(req).await.expect_err("must fail");
        match err {
            DomainError::ShellNotFound(id) => assert_eq!(id.as_str(), "absent"),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    // --- execute: matched ------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn single_pattern_matched_returns_matched_outcome() {
        let (uc, shells, streams, _clock) = build_use_case();
        let entity = seed_shell(&shells, "sh-1").await;
        streams.put_shell(&entity.id, snapshot(b"login: alice\n$ ", 3_u64));
        let req = base_request(&entity.id, vec!["$ ".to_string()]);
        let out = unwrap_outcome(uc.execute(req).await);
        assert_eq!(out.status, WaitForPatternStatus::Matched);
        assert_eq!(out.matched_pattern.as_deref(), Some("$ "));
        assert_eq!(out.data.as_ref(), b"login: alice\n$ ");
        assert_eq!(out.last_seq, 3_u64);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn multi_pattern_earliest_match_wins() {
        let (uc, shells, streams, _clock) = build_use_case();
        let entity = seed_shell(&shells, "sh-2").await;
        // "world" at 6, "hello" at 0 — earliest wins.
        streams.put_shell(&entity.id, snapshot(b"hello world", 1_u64));
        let req = base_request(&entity.id, vec!["world".to_string(), "hello".to_string()]);
        let out = unwrap_outcome(uc.execute(req).await);
        assert_eq!(out.status, WaitForPatternStatus::Matched);
        assert_eq!(out.matched_pattern.as_deref(), Some("hello"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tie_at_position_zero_first_pattern_in_list_wins() {
        let (uc, shells, streams, _clock) = build_use_case();
        let entity = seed_shell(&shells, "sh-3").await;
        streams.put_shell(&entity.id, snapshot(b"alphabet rocks", 1_u64));
        let req = base_request(
            &entity.id,
            vec!["alpha".to_string(), "alphabet".to_string()],
        );
        let out = unwrap_outcome(uc.execute(req).await);
        assert_eq!(out.matched_pattern.as_deref(), Some("alpha"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn matched_outcome_truncates_data_to_max_output_bytes_tail() {
        let (uc, shells, streams, _clock) = build_use_case();
        let entity = seed_shell(&shells, "sh-trunc").await;
        streams.put_shell(
            &entity.id,
            snapshot(b"AAAAAAAAAAAAAAAAAAAAAAAATAILSEEN", 4_u64),
        );
        let mut req = base_request(&entity.id, vec!["TAIL".to_string()]);
        req.max_output_bytes = Some(8_usize);
        let out = unwrap_outcome(uc.execute(req).await);
        assert_eq!(out.status, WaitForPatternStatus::Matched);
        assert_eq!(out.data.len(), 8_usize);
        assert_eq!(out.data.as_ref(), b"TAILSEEN");
    }

    // --- execute: timeout ------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn no_match_within_budget_returns_timeout_with_current_buffer() {
        let (uc, shells, streams, _clock) = build_use_case();
        let entity = seed_shell(&shells, "sh-stuck").await;
        streams.put_shell(&entity.id, snapshot(b"partial-output", 2_u64));
        let mut req = base_request(&entity.id, vec!["never-appears".to_string()]);
        req.timeout = Some(Duration::from_millis(120));
        let started = std::time::Instant::now();
        let out = unwrap_outcome(uc.execute(req).await);
        let elapsed = started.elapsed();
        assert_eq!(out.status, WaitForPatternStatus::Timeout);
        assert!(out.matched_pattern.is_none());
        assert_eq!(out.data.as_ref(), b"partial-output");
        assert_eq!(out.last_seq, 2_u64);
        assert!(
            elapsed >= Duration::from_millis(100),
            "must have waited at least the budget (got {elapsed:?})"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "must not exceed the budget by orders of magnitude (got {elapsed:?})"
        );
    }

    // --- execute: closed -------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn shell_closed_during_wait_returns_closed_status() {
        let (uc, shells, streams, _clock) = build_use_case();
        let entity = seed_shell(&shells, "sh-closing").await;
        streams.put_shell(&entity.id, snapshot(b"some output", 5_u64));
        // Flip the shell to closed in the background after one poll tick.
        let shells_bg = Arc::clone(&shells);
        let id_bg = entity.id.clone();
        let task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(80)).await;
            let mut updated = shells_bg
                .get(&id_bg)
                .await
                .expect("bg get")
                .expect("entity present");
            updated = updated.close();
            shells_bg.update(updated).await.expect("bg update");
        });
        let req = base_request(&entity.id, vec!["never".to_string()]);
        let out = unwrap_outcome(uc.execute(req).await);
        task.await.expect("bg task join");
        match out.status {
            WaitForPatternStatus::Closed => {}
            WaitForPatternStatus::Matched | WaitForPatternStatus::Timeout => {
                panic!("expected Closed, got {:?}", out.status);
            }
        }
        assert!(out.matched_pattern.is_none());
        assert_eq!(out.data.as_ref(), b"some output");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shell_already_closed_returns_closed_immediately() {
        let (uc, shells, streams, _clock) = build_use_case();
        let entity = seed_shell(&shells, "sh-pre-closed").await;
        // Pre-close the shell.
        let closed = entity.clone().close();
        shells.update(closed).await.expect("pre-close");
        streams.put_shell(&entity.id, snapshot(b"final", 1_u64));
        let req = base_request(&entity.id, vec!["never".to_string()]);
        let out = unwrap_outcome(uc.execute(req).await);
        assert_eq!(out.status, WaitForPatternStatus::Closed);
        assert_eq!(out.data.as_ref(), b"final");
    }

    // --- execute: timeout cap --------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn timeout_above_cap_is_clamped_to_300_seconds() {
        // We only assert the cap does not produce a panic / overflow. The
        // wait loop is bounded by the buffer scan finding the pattern
        // immediately so the test stays sub-second.
        let (uc, shells, streams, _clock) = build_use_case();
        let entity = seed_shell(&shells, "sh-cap").await;
        streams.put_shell(&entity.id, snapshot(b"matched-prompt$ ", 1_u64));
        let mut req = base_request(&entity.id, vec!["$ ".to_string()]);
        req.timeout = Some(Duration::from_secs(36_000_u64));
        let out = unwrap_outcome(uc.execute(req).await);
        assert_eq!(out.status, WaitForPatternStatus::Matched);
    }
}
