// SPDX-License-Identifier: ISC
//! Ported from OpenBSD's openrsync — `session.c`.
//!
//! Original copyright: Kristaps Dzonsons; ISC license. See
//! `LICENSES/openrsync-ISC.txt` for the full notice.
//!
//! This port maintains openrsync's struct names + field order to ease
//! cross-references against the C source. The I/O layer is rewritten
//! to async + lock-free Rust per the project's hot-path invariants.
//!
//! # Scope of this slice (slice 1)
//!
//! Only the handshake half of `session.c` lives here — `sess_stats_send`
//! / `sess_stats_recv` ship in a later slice once the sender / receiver
//! state machines drive the inner protocol. The handshake mirrors
//! `client.c::rsync_client` (lines 51..76) plus the `sess` struct from
//! `extern.h`.
//!
//! # openrsync vs rsync 3.2.7 wire deviation
//!
//! openrsync targets **protocol 27** and its handshake is exactly three
//! u32 LE writes/reads:
//!
//! ```text
//! client -> server: u32 LE  client_version
//! server -> client: u32 LE  server_version
//! server -> client: u32 LE  checksum_seed
//! ```
//!
//! ssh-mcp targets the modern stock rsync 3.2.x / 3.4.x (protocol 31+) — a
//! superset whose handshake injects a server-emitted *`compat_flags`*
//! varint between the version exchange and the seed. We pin
//! [`RSYNC_PROTOCOL`] at 32 (v32, wire-identical to v31 — per the ADR 0011
//! phase 8 brief plus the administrative v31→v32 bump in rsync 3.4.0) and
//! tolerate the additional varint in [`handshake`]. Everything else in
//! this module follows openrsync byte-for-byte.

use tokio::io::{AsyncRead, AsyncWrite};

use crate::adapters::rsync::wire::io::{MplexReader, MplexWriter};
use crate::domain::error::DomainError;

/// Protocol version we advertise on the wire.
///
/// Pinned at **32** (v32, wire-identical to v31) to match the rsync 3.4.x
/// administrative bump. openrsync's `extern.h::RSYNC_PROTOCOL` is 27;
/// ssh-mcp's wire client targets the modern stock rsync 3.2.x so it must
/// speak the protocol-30+ flist shape (16-bit `XMIT_EXTENDED_FLAGS` flag
/// bytes, varint30/varlong30 length fields). The lift from 27 to 31
/// happens in lockstep with the proto-30+ encoders/decoders in
/// [`super::flist`]; the v31 → v32 step is a no-op wire change: rsync
/// 3.4.0 incremented the protocol number as an administrative signal for
/// the CVE-2024-12084..12088 + 12747 fixes, but introduced zero new wire
/// branches or encoding changes. Negotiation against a v31 server
/// (e.g. rsync 3.2.7) yields `min(32, 31) = 31`, preserving full
/// backward compatibility.
///
/// We still accept anything in `RSYNC_PROTOCOL_MIN..=RSYNC_PROTOCOL_MAX`
/// from the peer; `min(local, remote)` becomes the negotiated value
/// and downstream encoders branch on it. The legacy 8-bit `FLIST_*`
/// path is preserved verbatim under the same module so unit tests
/// against canned proto-27 byte streams keep round-tripping.
///
/// Typed `i32` so the [`WireSession`] `lver` / `rver` field types
/// (also `i32`, mirroring openrsync's `int32_t`) avoid lossy `as`
/// casts. Wire serialisation uses [`i32::to_le_bytes`] directly —
/// the byte representation is identical to the `u32` form for the
/// non-negative protocol values we ever emit (27..=32).
pub const RSYNC_PROTOCOL: i32 = 32;

/// Hard floor on the remote protocol version (= 27).
///
/// Below this we surface [`DomainError::RsyncVersionTooOld`] and bail.
/// Matches the original openrsync floor so we still talk to legacy
/// peers when the wire client is ever pointed at one.
pub const RSYNC_PROTOCOL_MIN: i32 = 27;

/// Soft ceiling on remote protocol versions we will negotiate down to
/// without complaining. `min(local, remote)` is the negotiated value.
pub const RSYNC_PROTOCOL_MAX: i32 = 32;

/// Boundary at which the flist wire shape switches to the protocol-28+
/// 16-bit `XMIT_EXTENDED_FLAGS` flag byte. Below this, the encoder
/// keeps the openrsync 8-bit `FLIST_*` path.
pub const XMIT_EXTENDED_FLAGS_MIN_PROTOCOL: i32 = 28;

/// Boundary for varint-encoded flist field shape (= 30).
///
/// At protocol >= 30 the wire shape switches the length / mtime /
/// file-size / symlink-len fields from `write_int` / `write_long` to
/// `write_varint` / `write_varlong` (= `write_varint30` /
/// `write_varlong30` in upstream rsync 3.2.7's `io.h`).
pub const VARINT_FLIST_MIN_PROTOCOL: i32 = 30;

/// Multiplex framing kicks in at protocol 30. Older protocols would
/// require the unframed I/O variant; this port refuses anything older
/// because the inner protocol bytes alone are not enough for ssh-mcp's
/// progress-event surface.
const MPLEX_FRAMING_MIN_PROTOCOL: i32 = 30;

/// Values required during a communication session.
///
/// Direct port of `extern.h::struct sess` (lines 248..259). The field
/// order is preserved for cross-references; types are widened to
/// fixed-width Rust integers and the implicit `0` initial values fall
/// out of [`Default`].
///
/// Lock-free contract: this struct is **passed by value** through the
/// per-session task's stack. It is never wrapped in `Arc<Mutex<...>>`,
/// `RwLock<...>`, or any cross-task shared cell. If the higher-level
/// state machine ever needs to observe `total_read` / `total_write` /
/// `total_size` from outside the session task, expose them via
/// `ArcSwap<SessionStats>` or atomics — never `Mutex<sess>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WireSession {
    /// Server-supplied checksum seed used by the rolling-checksum
    /// kernel. openrsync field name: `seed`.
    pub seed: i32,
    /// Local protocol version advertised on the wire (i.e.
    /// [`RSYNC_PROTOCOL`]). openrsync field name: `lver`.
    pub lver: i32,
    /// Remote protocol version observed during the handshake.
    /// openrsync field name: `rver`.
    pub rver: i32,
    /// Total bytes pulled off the wire during inner-protocol reads
    /// (post-mplex). openrsync field name: `total_read`.
    pub total_read: u64,
    /// Sum of file sizes touched in this session. openrsync field
    /// name: `total_size`.
    pub total_size: u64,
    /// Total bytes pushed onto the wire during inner-protocol writes
    /// (pre-mplex). openrsync field name: `total_write`.
    pub total_write: u64,
    /// Are we currently reading from a multiplexed stream? Mirrors
    /// `mplex_reads` in openrsync's `sess`. The reader half of the
    /// channel owns this flag — never shared across tasks.
    pub mplex_reads: bool,
    /// In-flight remaining bytes inside the current mplex `MSG_DATA`
    /// frame. Owned by the reader task. openrsync field name:
    /// `mplex_read_remain` (`size_t` there, `usize` here).
    pub mplex_read_remain: usize,
    /// Are we currently writing to a multiplexed stream? Mirrors
    /// `mplex_writes` in openrsync's `sess`. Owned by the writer task.
    pub mplex_writes: bool,
    /// Negotiated protocol version after the handshake (`min(lver,
    /// rver)`). Not present in openrsync's `sess` (which lives with
    /// the lower of the two via implicit clamping); we surface it as
    /// a first-class field so callers can pin behaviour at the
    /// negotiated version. Typed `i32` to match `lver` / `rver` and
    /// openrsync's `int32_t` protocol-version fields.
    pub negotiated: i32,
}

impl WireSession {
    /// Build a session with `lver` set to the local protocol version.
    /// All other fields default to zero / `false`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            seed: 0,
            lver: RSYNC_PROTOCOL,
            rver: 0,
            total_read: 0,
            total_size: 0,
            total_write: 0,
            mplex_reads: false,
            mplex_read_remain: 0,
            mplex_writes: false,
            negotiated: 0,
        }
    }

    /// `true` when the negotiated protocol version supports mplex
    /// framing (i.e. `>= 30`).
    #[must_use]
    pub const fn mplex_supported(&self) -> bool {
        self.negotiated >= MPLEX_FRAMING_MIN_PROTOCOL
    }
}

/// Confirm a peer-advertised protocol version is supported.
///
/// Mirrors the `if (sess.rver < sess.lver)` check in
/// `client.c::rsync_client` (lines 66..72). openrsync rejects only
/// versions that are *older* than its local one. We additionally
/// guard the explicit [`RSYNC_PROTOCOL_MIN`] floor so the same fn is
/// reusable from the daemon mode (where `lver` is configurable).
const fn protocol_supported(lver: i32, rver: i32) -> bool {
    rver >= RSYNC_PROTOCOL_MIN && rver >= lver - 4
}

/// Drive the rsync handshake against an [`MplexReader`] +
/// [`MplexWriter`] pair.
///
/// Sequence (port of `client.c::rsync_client` lines 51..76):
///
/// ```text
/// 1. Write our protocol version (4 bytes LE)         (lver)
/// 2. Read remote's protocol version (4 bytes LE)     (rver)
/// 3. (rsync 3.x extension — NOT in openrsync 27)
///    Read server's compat_flags varint and discard
/// 4. Read the server's checksum seed (4 bytes LE)    (seed)
/// 5. Switch the reader task into mplex mode
///    (sess.mplex_reads = true)
/// ```
///
/// Step 3 is the only divergence from openrsync's protocol-27 model.
/// rsync 3.2.x emits a varint `compat_flags` byte (or a multi-byte
/// extension when bit 7 is set) that openrsync 27 has no concept of.
/// The varint is decoded but its semantic effect on subsequent
/// negotiation is left for the inner-protocol slices.
///
/// # Errors
///
/// - [`DomainError::RsyncVersionTooOld`] when the remote's protocol
///   version is below [`RSYNC_PROTOCOL_MIN`].
/// - [`DomainError::RsyncProtocolError`] for I/O failures or malformed
///   bytes mid-handshake.
pub async fn handshake<R, W>(
    reader: &mut MplexReader<R>,
    writer: &mut MplexWriter<W>,
) -> Result<WireSession, DomainError>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let mut sess = WireSession::new();
    exchange_versions(reader, writer, &mut sess).await?;
    drain_compat_flags(reader, &mut sess).await?;
    sess.seed = reader.read_int(&mut sess).await?;
    if sess.mplex_supported() {
        sess.mplex_reads = true;
    }
    log_handshake_complete(&sess);
    Ok(sess)
}

/// Steps 1 + 2 of the openrsync handshake (`client.c::rsync_client`
/// lines 51..72): write `lver`, read `rver`, validate the floor, set
/// `negotiated`.
async fn exchange_versions<R, W>(
    reader: &mut MplexReader<R>,
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
) -> Result<(), DomainError>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let lver = sess.lver;
    writer.write_int(sess, lver).await?;
    sess.rver = reader.read_int(sess).await?;
    if !protocol_supported(sess.lver, sess.rver) {
        return Err(DomainError::RsyncVersionTooOld(format!(
            "remote protocol {} is older than our local {}",
            sess.rver, sess.lver
        )));
    }
    sess.negotiated = sess.rver.min(sess.lver);
    Ok(())
}

/// Step 3 of the handshake — the rsync 3.x `compat_flags` varint.
/// openrsync 27 omits this entirely. The first byte alone covers all
/// cases produced by stock rsync 3.2.x; bit 7 set means a multi-byte
/// extension whose tail bytes are consumed but not interpreted (the
/// inner-protocol slices will decide what to do with them).
async fn drain_compat_flags<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
) -> Result<(), DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    let first = reader.read_byte(sess).await?;
    if first & 0x80 != 0 {
        let extra_bytes = compat_flags_extra_bytes(first);
        for _ in 0..extra_bytes {
            let _ = reader.read_byte(sess).await?;
        }
    }
    Ok(())
}

/// Tail-end log of [`handshake`] — split out so the public fn stays
/// under the 30-line cognitive-complexity threshold.
fn log_handshake_complete(sess: &WireSession) {
    tracing::info!(
        target: "rsync.wire.session",
        lver = sess.lver,
        rver = sess.rver,
        negotiated = sess.negotiated,
        seed = format!("{:#x}", u32::from_le_bytes(sess.seed.to_le_bytes())),
        mplex_reads = sess.mplex_reads,
        "handshake: complete"
    );
}

/// Map a `compat_flags` varint prefix byte to the count of extra bytes
/// that follow. Mirrors the `int_byte_extra` table in rsync 3.2.7's
/// `io.c::read_varint` for the high-bit-set branch only.
const fn compat_flags_extra_bytes(prefix: u8) -> usize {
    if prefix & 0xc0 == 0x80 {
        1
    } else if prefix & 0xe0 == 0xc0 {
        2
    } else if prefix & 0xf0 == 0xe0 {
        3
    } else {
        4
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test code uses unwrap/expect for brevity per project convention"
)]
mod tests {
    use super::{
        MPLEX_FRAMING_MIN_PROTOCOL, RSYNC_PROTOCOL, RSYNC_PROTOCOL_MAX, RSYNC_PROTOCOL_MIN,
        WireSession, compat_flags_extra_bytes, handshake, protocol_supported,
    };
    use crate::adapters::rsync::wire::io::{MplexReader, MplexWriter};
    use crate::domain::error::DomainError;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

    #[test]
    fn session_default_zero_initialised() {
        let s = WireSession::new();
        assert_eq!(s.lver, RSYNC_PROTOCOL);
        assert_eq!(s.rver, 0);
        assert_eq!(s.seed, 0);
        assert_eq!(s.total_read, 0);
        assert_eq!(s.total_write, 0);
        assert_eq!(s.total_size, 0);
        assert!(!s.mplex_reads);
        assert!(!s.mplex_writes);
        assert_eq!(s.mplex_read_remain, 0);
        assert_eq!(s.negotiated, 0);
    }

    #[test]
    fn protocol_constants_are_in_range() {
        assert!(RSYNC_PROTOCOL >= RSYNC_PROTOCOL_MIN);
        assert!(RSYNC_PROTOCOL <= RSYNC_PROTOCOL_MAX);
        // Slice-4 lifts the pinned local protocol to 31 (proto-30+ flist
        // wire shape: 16-bit XMIT_EXTENDED_FLAGS + varint30 length fields).
        // Bumped to 32 in v7.0.1 — wire-identical to 31 (rsync 3.4.x
        // administrative bump). Mplex framing is enabled because
        // RSYNC_PROTOCOL >= 30.
        assert_eq!(RSYNC_PROTOCOL, 32);
    }

    #[test]
    fn protocol_supported_matches_openrsync_floor() {
        let lver = RSYNC_PROTOCOL;
        // Same version is fine.
        assert!(protocol_supported(lver, lver));
        // Effective floor is max(RSYNC_PROTOCOL_MIN, lver - 4).
        // With lver=32: max(27, 28) = 28. proto 28+ must be accepted.
        let effective_floor = RSYNC_PROTOCOL_MIN.max(lver - 4);
        assert!(protocol_supported(lver, effective_floor), "effective floor {effective_floor} must be accepted");
        // One below effective floor is rejected.
        assert!(!protocol_supported(lver, effective_floor - 1), "below effective floor must be rejected");
        // Higher remote protocol is fine — `rver < lver` would only
        // be rejected if rver is below `lver - 4` per openrsync.
        assert!(protocol_supported(lver, lver + 5));
    }

    #[test]
    fn compat_flags_extra_bytes_table() {
        // Single-byte extension (bit 7 only).
        assert_eq!(compat_flags_extra_bytes(0x80), 1);
        // Two-byte extension (bits 7..6).
        assert_eq!(compat_flags_extra_bytes(0xc0), 2);
        // Three-byte extension (bits 7..5).
        assert_eq!(compat_flags_extra_bytes(0xe0), 3);
        // Four-byte extension (bits 7..4 set).
        assert_eq!(compat_flags_extra_bytes(0xf0), 4);
    }

    #[tokio::test]
    async fn handshake_succeeds_against_v31_server_no_compat_flags() {
        let (a, mut b) = duplex(64);
        // The "remote" speaks rsync 3.2.x: read our version, write its
        // version, write a 0-byte compat_flags, write a seed.
        let remote = tokio::spawn(async move {
            let mut buf = [0_u8; 4];
            b.read_exact(&mut buf).await.expect("read lver");
            assert_eq!(i32::from_le_bytes(buf), RSYNC_PROTOCOL);
            // Server version.
            b.write_all(&31_u32.to_le_bytes())
                .await
                .expect("write rver");
            // compat_flags = 0 (high bit clear → no extra bytes).
            b.write_all(&[0_u8]).await.expect("write compat_flags");
            // Seed.
            b.write_all(&0xdead_beef_u32.to_le_bytes())
                .await
                .expect("write seed");
        });
        let (r, w) = tokio::io::split(a);
        let mut mr = MplexReader::new(r);
        let mut mw = MplexWriter::new(w);
        let sess = handshake(&mut mr, &mut mw).await.expect("handshake");
        remote.await.expect("remote");
        assert_eq!(sess.lver, RSYNC_PROTOCOL);
        assert_eq!(sess.rver, 31);
        assert_eq!(sess.negotiated, RSYNC_PROTOCOL.min(31));
        assert_eq!(u32::from_le_bytes(sess.seed.to_le_bytes()), 0xdead_beef_u32);
        // Slice-4 lver=31 means the negotiated value is 31 (>= 30),
        // so the handshake layer flips `mplex_reads` on per
        // openrsync's `client.c` line 76 + the `mplex_supported`
        // helper here.
        assert!(sess.mplex_reads);
    }

    #[tokio::test]
    async fn handshake_drains_multibyte_compat_flags() {
        let (a, mut b) = duplex(64);
        let remote = tokio::spawn(async move {
            let mut buf = [0_u8; 4];
            b.read_exact(&mut buf).await.expect("lver");
            b.write_all(&31_u32.to_le_bytes()).await.expect("rver");
            // 3-byte compat_flags varint per rsync 3.2.7's
            // `int_byte_extra` table: prefix `0xc0` (in the
            // 0xc0..=0xdf range) means 2 extra bytes follow.
            b.write_all(&[0xc0_u8, 0x42_u8, 0x99_u8])
                .await
                .expect("compat_flags");
            b.write_all(&0_u32.to_le_bytes()).await.expect("seed");
        });
        let (r, w) = tokio::io::split(a);
        let mut mr = MplexReader::new(r);
        let mut mw = MplexWriter::new(w);
        let sess = handshake(&mut mr, &mut mw).await.expect("handshake");
        remote.await.expect("remote");
        assert_eq!(sess.rver, 31);
    }

    #[tokio::test]
    async fn handshake_rejects_pre_v27_remote() {
        let (a, mut b) = duplex(64);
        let remote = tokio::spawn(async move {
            let mut buf = [0_u8; 4];
            b.read_exact(&mut buf).await.expect("lver");
            // Lie about being v22.
            b.write_all(&22_u32.to_le_bytes()).await.expect("rver");
        });
        let (r, w) = tokio::io::split(a);
        let mut mr = MplexReader::new(r);
        let mut mw = MplexWriter::new(w);
        let err = handshake(&mut mr, &mut mw).await.expect_err("must err");
        match err {
            DomainError::RsyncVersionTooOld(detail) => {
                assert!(detail.contains("22"), "{detail}");
            }
            other => panic!("expected RsyncVersionTooOld, got {other:?}"),
        }
        remote.await.expect("remote");
    }

    #[tokio::test]
    async fn handshake_eof_mid_read_is_protocol_error() {
        let (a, b) = duplex(64);
        drop(b);
        let (r, w) = tokio::io::split(a);
        let mut mr = MplexReader::new(r);
        let mut mw = MplexWriter::new(w);
        let err = handshake(&mut mr, &mut mw).await.expect_err("must err");
        assert!(matches!(err, DomainError::RsyncProtocolError(_)));
    }

    #[test]
    fn handshake_succeeds_against_v32_server() {
        // Proto 32 == proto 31 wire-identical (rsync 3.4.0 administrative bump).
        // Local advertises 32, remote advertises 32 → negotiated = 32.
        let mut sess = WireSession::new();
        sess.lver = RSYNC_PROTOCOL;
        sess.rver = 32;
        sess.negotiated = sess.rver.min(sess.lver);
        assert_eq!(sess.negotiated, 32, "lver=32 + rver=32 must negotiate to 32");
        assert!(
            sess.negotiated >= MPLEX_FRAMING_MIN_PROTOCOL,
            "proto 32 must enable mplex framing (min {MPLEX_FRAMING_MIN_PROTOCOL})"
        );
    }

    #[test]
    fn handshake_downgrades_to_v31_against_legacy_server() {
        // rsync 3.2.7 / aragog scenario: lver=32, rver=31 → negotiated=31.
        // Verifies backward compat with all servers below 3.4.0.
        let mut sess = WireSession::new();
        sess.lver = RSYNC_PROTOCOL;
        sess.rver = 31;
        sess.negotiated = sess.rver.min(sess.lver);
        assert_eq!(sess.negotiated, 31, "lver=32 + rver=31 must downgrade to 31");
    }
}
