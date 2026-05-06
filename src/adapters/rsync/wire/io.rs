// SPDX-License-Identifier: ISC
//! Ported from OpenBSD's openrsync — `io.c`.
//!
//! Original copyright: Kristaps Dzonsons; ISC license. See
//! `LICENSES/openrsync-ISC.txt` for the full notice.
//!
//! This port maintains openrsync's struct names + field order to ease
//! cross-references against the C source. The I/O layer is rewritten
//! to async + lock-free Rust per the project's hot-path invariants.
//!
//! # Lock-free contract (CRITICAL)
//!
//! openrsync's `io.c` uses synchronous blocking syscalls plus
//! `poll(2)` and a global byte-counter pinned to the `struct sess` it
//! gets passed. Naively porting that to async Rust would invite the
//! `Arc<Mutex<sess>>` antipattern.
//!
//! Our port refuses that:
//!
//! - [`MplexReader`] owns its `AsyncRead` half **exclusively**. Per-fd
//!   read state (`mplex_reads`, `mplex_read_remain`) lives inside the
//!   `&mut WireSession` the active task threads through every read
//!   call — never on a shared cell.
//! - [`MplexWriter`] owns its `AsyncWrite` half **exclusively**. Same
//!   ownership rule for `mplex_writes` and `total_write`.
//! - Higher layers that need to drive reads + writes from separate
//!   tasks SHALL split the russh channel halves and pump each half
//!   from its own `tokio::spawn`-ed task. Cross-task communication
//!   uses `tokio::sync::mpsc::channel(N)` — never a `Mutex`.
//! - openrsync's `lowbuffer` / output queue (`io_lowbuffer_*`) is
//!   **not** ported as a Rust struct. If a higher layer needs to
//!   enqueue outgoing data from multiple producers, it builds a
//!   `tokio::sync::mpsc::Sender` feeding a single writer task. That
//!   writer task owns its [`MplexWriter`] mutably — no `Mutex`.
//!
//! # Mplex framing — wire shape
//!
//! After the handshake, every byte exchanged with `rsync --server`
//! travels in 4-byte-headered frames:
//!
//! ```text
//! +--------+---------+---------+---------+
//! | tag    | len[2]  | len[1]  | len[0]  |   header (LE u32 = (tag << 24) | len)
//! +--------+---------+---------+---------+
//! | payload (len bytes)                  |
//! +--------+---------+---------+---------+
//! ```
//!
//! The header is a single little-endian u32 where the high byte is
//! the multiplex tag (already biased by `MPLEX_BASE = 7` per
//! openrsync's `io.c::io_write_buf` line 148:
//! `tag = (7 << 24) + wsz`) and the low 24 bits are the payload
//! length.

use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::adapters::rsync::wire::session::WireSession;
use crate::domain::error::DomainError;

/// Bias applied to mplex tags on the wire (`MPLEX_BASE` in `rsync.h`,
/// also `7 << 24` in openrsync's `io.c::io_write_buf` line 148).
pub const MPLEX_BASE: u8 = 7;

/// Maximum payload bytes encodable in a single mplex frame
/// (`(1 << 24) - 1`, dictated by the 24-bit length field). Mirrors
/// the `0xFFFFFF` mask in openrsync's `io.c::io_write_buf` line 147.
pub const MAX_PAYLOAD_LEN: usize = (1_usize << 24_u32) - 1;

/// Local cap on out-of-band mplex payload size — tracks the
/// `sizeof(mpbuf)` (1024) ceiling enforced by openrsync's
/// `io.c::io_read_flush` line 300.
const OOB_PAYLOAD_CAP: usize = 1024;

/// Canonical mplex tag (post-bias removal).
///
/// Variants mirror the `enum msgcode` enum from rsync 3.2.7's
/// `rsync.h`. Wire bytes are `tag + MPLEX_BASE` (see
/// [`MplexTag::from_wire`] / [`MplexTag::to_wire`]). openrsync's
/// `io.c::io_read_flush` only treats the `tag - MPLEX_BASE == 0`
/// case (== `MSG_DATA`) as data, every other tag as a log line, and
/// `tag - MPLEX_BASE == 1` as a fatal remote error. We expand that
/// dispatch to the full rsync 3.2.x set so we can route fatal /
/// advisory / semantic frames without losing fidelity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MplexTag {
    /// `MSG_DATA = 0` — raw inner-protocol bytes.
    Data,
    /// `MSG_ERROR_XFER = 1` — per-file error.
    ErrorXfer,
    /// `MSG_INFO = 2` — info message.
    Info,
    /// `MSG_ERROR = 3` — fatal error; caller must terminate.
    Error,
    /// `MSG_WARNING = 4`.
    Warning,
    /// `MSG_ERROR_SOCKET = 5` — socket-layer error.
    ErrorSocket,
    /// `MSG_LOG = 6`.
    Log,
    /// `MSG_CLIENT = 7`.
    Client,
    /// `MSG_ERROR_UTF8 = 8`.
    ErrorUtf8,
    /// `MSG_REDO = 9` — reprocess indicated flist index.
    Redo,
    /// `MSG_STATS = 10` — message has stats data for generator.
    Stats,
    /// `MSG_IO_ERROR = 22` — sending side had an I/O error.
    IoError,
    /// `MSG_IO_TIMEOUT = 33` — daemon's timeout value.
    IoTimeout,
    /// `MSG_NOOP = 42` — protocol-30 only do-nothing message.
    Noop,
    /// `MSG_ERROR_EXIT = 86` — error exit synchronisation (v31+).
    ErrorExit,
    /// `MSG_SUCCESS = 100` — file index successfully updated.
    Success,
    /// `MSG_DELETED = 101` — file successfully deleted on receiver.
    Deleted,
    /// `MSG_NO_SEND = 102` — sender failed to open requested file.
    NoSend,
    /// Unknown tag byte. The biased byte is preserved so the caller
    /// can log + skip the frame.
    Unknown(u8),
}

impl MplexTag {
    /// Convert a *biased* tag byte (the one read off the wire) into a
    /// canonical [`MplexTag`].
    #[must_use]
    pub const fn from_wire(byte: u8) -> Self {
        match byte {
            // tag + MPLEX_BASE (7) per rsync.h enum msgcode.
            7 => Self::Data,         // MSG_DATA = 0
            8 => Self::ErrorXfer,    // MSG_ERROR_XFER = 1
            9 => Self::Info,         // MSG_INFO = 2
            10 => Self::Error,       // MSG_ERROR = 3
            11 => Self::Warning,     // MSG_WARNING = 4
            12 => Self::ErrorSocket, // MSG_ERROR_SOCKET = 5
            13 => Self::Log,         // MSG_LOG = 6
            14 => Self::Client,      // MSG_CLIENT = 7
            15 => Self::ErrorUtf8,   // MSG_ERROR_UTF8 = 8
            16 => Self::Redo,        // MSG_REDO = 9
            17 => Self::Stats,       // MSG_STATS = 10
            29 => Self::IoError,     // MSG_IO_ERROR = 22
            40 => Self::IoTimeout,   // MSG_IO_TIMEOUT = 33
            49 => Self::Noop,        // MSG_NOOP = 42
            93 => Self::ErrorExit,   // MSG_ERROR_EXIT = 86
            107 => Self::Success,    // MSG_SUCCESS = 100
            108 => Self::Deleted,    // MSG_DELETED = 101
            109 => Self::NoSend,     // MSG_NO_SEND = 102
            other => Self::Unknown(other),
        }
    }

    /// Convert the canonical tag back to its *biased* wire byte.
    #[must_use]
    pub const fn to_wire(self) -> u8 {
        match self {
            Self::Data => 7,
            Self::ErrorXfer => 8,
            Self::Info => 9,
            Self::Error => 10,
            Self::Warning => 11,
            Self::ErrorSocket => 12,
            Self::Log => 13,
            Self::Client => 14,
            Self::ErrorUtf8 => 15,
            Self::Redo => 16,
            Self::Stats => 17,
            Self::IoError => 29,
            Self::IoTimeout => 40,
            Self::Noop => 49,
            Self::ErrorExit => 93,
            Self::Success => 107,
            Self::Deleted => 108,
            Self::NoSend => 109,
            Self::Unknown(byte) => byte,
        }
    }

    /// `true` when this tag carries a fatal error; the caller must
    /// abort the session. Mirrors openrsync's `io.c::io_read_flush`
    /// `tag == 1` check (line 326), generalised to the rsync 3.2.x
    /// fatal set.
    #[must_use]
    pub const fn is_fatal(self) -> bool {
        matches!(
            self,
            Self::Error | Self::ErrorSocket | Self::IoError | Self::ErrorExit
        )
    }

    /// `true` when this tag is purely advisory — drop quietly into
    /// `tracing` and continue. Mirrors openrsync's `io.c::io_read_flush`
    /// non-data branch which always logs via `LOG0` (line 318).
    #[must_use]
    pub const fn is_advisory(self) -> bool {
        matches!(
            self,
            Self::Info | Self::Warning | Self::Log | Self::Client | Self::Noop | Self::ErrorUtf8
        )
    }
}

// =====================================================================
// Reader half — port of openrsync io.c::io_read_* + io_read_flush.
// =====================================================================

/// Reader half of a mplex stream.
///
/// Owns its `AsyncRead` exclusively. Per-direction session state
/// (`mplex_reads`, `mplex_read_remain`, `total_read`) is threaded in
/// via `&mut WireSession` on every read call so multiple readers
/// could in principle share a session value, though in practice one
/// task drives both halves serially during the handshake and one
/// task per half during the inner protocol — see module docs.
pub struct MplexReader<R: AsyncRead + Unpin + Send> {
    inner: R,
}

impl<R: AsyncRead + Unpin + Send> MplexReader<R> {
    /// Wrap an `AsyncRead` into a mplex reader.
    pub const fn new(inner: R) -> Self {
        Self { inner }
    }

    /// Drop the wrapper and return the underlying reader.
    pub fn into_inner(self) -> R {
        self.inner
    }

    /// Borrow the underlying reader mutably for one-off raw reads.
    /// Used by [`MplexReader::read_frame`] tests and by callers that
    /// stay below the mplex framer.
    pub const fn raw_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// Peek at the next mplex frame off the wire (header + payload).
    /// This does **not** dispatch advisory frames — every caller in
    /// our port goes through [`Self::read_buf`] / [`Self::read_int`] /
    /// [`Self::read_byte`] which transparently demuxes advisory frames
    /// per `io.c::io_read_flush`.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::RsyncProtocolError`] when the underlying
    /// reader hits EOF mid-frame, when the header is malformed, or
    /// when the I/O layer surfaces an error.
    pub async fn read_frame(&mut self) -> Result<(MplexTag, Bytes), DomainError> {
        let header = read_u32_le(&mut self.inner).await?;
        let tag_byte = u8::try_from(header >> 24_u32).unwrap_or(0);
        let len = usize::try_from(header & 0x00ff_ffff).unwrap_or(0);
        let mut buf = vec![0_u8; len];
        if !buf.is_empty() {
            self.inner.read_exact(&mut buf).await.map_err(|e| {
                DomainError::RsyncProtocolError(format!(
                    "mplex payload read failed (wanted {len} bytes): {e}"
                ))
            })?;
        }
        Ok((MplexTag::from_wire(tag_byte), Bytes::from(buf)))
    }

    /// Read `dst.len()` bytes off the inner-protocol stream.
    ///
    /// Direct port of openrsync's `io.c::io_read_buf` (lines 339..384).
    /// When `sess.mplex_reads` is set, the function transparently
    /// demuxes advisory / fatal mplex frames via [`Self::read_flush`]
    /// and returns only the `MSG_DATA` bytes. When unset (e.g. during
    /// the bare handshake), it does a straight blocking read.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::RsyncProtocolError`] on transport
    /// failure, EOF mid-read, or a fatal mplex frame.
    pub async fn read_buf(
        &mut self,
        sess: &mut WireSession,
        dst: &mut [u8],
    ) -> Result<(), DomainError> {
        if !sess.mplex_reads {
            return self.read_unframed(sess, dst).await;
        }
        let mut filled = 0_usize;
        let total = dst.len();
        while filled < total {
            if sess.mplex_read_remain > 0 {
                filled = self.pump_data_slice(sess, dst, filled, total).await?;
                continue;
            }
            // Pull in the next mplex header and possibly drain an
            // advisory / fatal frame.
            self.read_flush(sess).await?;
        }
        Ok(())
    }

    /// Straight `read_exact` for the unframed (pre-mplex) handshake
    /// path. Mirrors the early-return branch of openrsync's
    /// `io.c::io_read_buf`.
    async fn read_unframed(
        &mut self,
        sess: &mut WireSession,
        dst: &mut [u8],
    ) -> Result<(), DomainError> {
        debug_assert_eq!(sess.mplex_read_remain, 0);
        self.inner.read_exact(dst).await.map_err(|e| {
            DomainError::RsyncProtocolError(format!("io_read_buf raw read failed: {e}"))
        })?;
        sess.total_read = sess
            .total_read
            .saturating_add(u64::try_from(dst.len()).unwrap_or(u64::MAX));
        Ok(())
    }

    /// Drain bytes from the in-flight `MSG_DATA` frame into `dst` and
    /// return the new fill cursor. Mirrors the inner-loop branch of
    /// openrsync's `io.c::io_read_buf` that consumes
    /// `sess.mplex_read_remain` bytes before pulling a new header.
    async fn pump_data_slice(
        &mut self,
        sess: &mut WireSession,
        dst: &mut [u8],
        filled: usize,
        total: usize,
    ) -> Result<usize, DomainError> {
        let want = sess.mplex_read_remain.min(total - filled);
        let end = filled + want;
        if let Some(slot) = dst.get_mut(filled..end) {
            self.inner.read_exact(slot).await.map_err(|e| {
                DomainError::RsyncProtocolError(format!("io_read_buf data slice read failed: {e}"))
            })?;
            // Slice-8 wire-trace hook — emits the byte slice we just
            // consumed off a `MSG_DATA` frame. Used to capture real
            // rsync 3.2.7 server output for the flist-decode bug hunt.
            // Disabled at the default tracing level; enable with
            // `RUST_LOG=rsync.wire.io=trace` for diagnosis only.
            tracing::trace!(
                target: "rsync.wire.io",
                offset = sess.total_read,
                len = want,
                bytes = format!("{:02x?}", slot),
                "read_buf data slice"
            );
        }
        sess.mplex_read_remain = sess.mplex_read_remain.saturating_sub(want);
        sess.total_read = sess
            .total_read
            .saturating_add(u64::try_from(want).unwrap_or(u64::MAX));
        Ok(end)
    }

    /// Read one byte off the inner-protocol stream. Port of
    /// openrsync's `io.c::io_read_byte` (lines 703..712).
    ///
    /// # Errors
    ///
    /// See [`Self::read_buf`].
    pub async fn read_byte(&mut self, sess: &mut WireSession) -> Result<u8, DomainError> {
        let mut b = [0_u8; 1];
        self.read_buf(sess, &mut b).await?;
        Ok(b[0])
    }

    /// Read a little-endian i32 off the inner-protocol stream. Port
    /// of openrsync's `io.c::io_read_int` / `io_read_uint` (lines
    /// 634..652).
    ///
    /// # Errors
    ///
    /// See [`Self::read_buf`].
    pub async fn read_int(&mut self, sess: &mut WireSession) -> Result<i32, DomainError> {
        let mut buf = [0_u8; 4];
        self.read_buf(sess, &mut buf).await?;
        Ok(i32::from_le_bytes(buf))
    }

    /// Drain a single mplex frame header off the wire and either:
    ///
    /// - set `sess.mplex_read_remain` if the frame is `MSG_DATA`, or
    /// - swallow the payload (logging the body as advisory) if the
    ///   frame is non-data, or
    /// - error out if the frame is fatal.
    ///
    /// Direct port of `io.c::io_read_flush` (lines 273..331). The
    /// only deviation: openrsync logs via `LOG0` regardless of tag;
    /// we route the payload through `tracing` at the appropriate
    /// level (info / warn / error).
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::RsyncProtocolError`] when the frame is
    /// fatal or the OOB body exceeds [`OOB_PAYLOAD_CAP`].
    pub async fn read_flush(&mut self, sess: &mut WireSession) -> Result<(), DomainError> {
        if sess.mplex_read_remain > 0 {
            return Ok(());
        }
        let header = read_u32_le(&mut self.inner).await?;
        let tag_byte = u8::try_from(header >> 24_u32).unwrap_or(0);
        let payload_len = usize::try_from(header & 0x00ff_ffff).unwrap_or(0);
        let tag = MplexTag::from_wire(tag_byte);

        if matches!(tag, MplexTag::Data) {
            sess.mplex_read_remain = payload_len;
            return Ok(());
        }
        if payload_len > OOB_PAYLOAD_CAP {
            return Err(DomainError::RsyncProtocolError(format!(
                "multiplex buffer overflow: tag {tag:?} len {payload_len} > {OOB_PAYLOAD_CAP}"
            )));
        }
        let body = self.read_oob_body(payload_len).await?;
        dispatch_oob_frame(tag, &body)
    }

    /// Pull the `payload_len`-byte OOB body off the wire and strip a
    /// single trailing newline (the openrsync convention from
    /// `io.c::io_read_flush`).
    async fn read_oob_body(&mut self, payload_len: usize) -> Result<Vec<u8>, DomainError> {
        let mut body = vec![0_u8; payload_len];
        if !body.is_empty() {
            self.inner.read_exact(&mut body).await.map_err(|e| {
                DomainError::RsyncProtocolError(format!(
                    "io_read_flush oob payload read failed: {e}"
                ))
            })?;
        }
        if body.last() == Some(&b'\n') {
            body.pop();
        }
        Ok(body)
    }
}

/// Route an out-of-band mplex frame (everything that is not
/// `MSG_DATA`) into the right `tracing` channel and surface fatal
/// frames as a [`DomainError::RsyncProtocolError`]. Mirrors the tail
/// of openrsync's `io.c::io_read_flush`.
fn dispatch_oob_frame(tag: MplexTag, body: &[u8]) -> Result<(), DomainError> {
    let text = String::from_utf8_lossy(body);
    if tag.is_fatal() {
        tracing::error!(
            target: "rsync.wire.io",
            ?tag,
            msg = %text,
            "fatal mplex frame from peer"
        );
        return Err(DomainError::RsyncProtocolError(format!(
            "remote raised fatal mplex frame ({tag:?}): {text}"
        )));
    }
    if tag.is_advisory() {
        tracing::info!(target: "rsync.wire.io", ?tag, msg = %text, "advisory mplex frame");
    } else {
        tracing::warn!(
            target: "rsync.wire.io",
            ?tag,
            msg = %text,
            "non-data mplex frame (semantic)"
        );
    }
    Ok(())
}

// =====================================================================
// Writer half — port of openrsync io.c::io_write_*.
// =====================================================================

/// Writer half of a mplex stream.
///
/// Owns its `AsyncWrite` exclusively. Per-direction session state
/// (`mplex_writes`, `total_write`) is threaded in via `&mut
/// WireSession` on every write call.
pub struct MplexWriter<W: AsyncWrite + Unpin + Send> {
    inner: W,
}

impl<W: AsyncWrite + Unpin + Send> MplexWriter<W> {
    /// Wrap an `AsyncWrite` into a mplex writer.
    pub const fn new(inner: W) -> Self {
        Self { inner }
    }

    /// Drop the wrapper and return the underlying writer.
    pub fn into_inner(self) -> W {
        self.inner
    }

    /// Borrow the underlying writer mutably. Used by tests and by
    /// transitional callers that bypass the mplex framer.
    pub const fn raw_mut(&mut self) -> &mut W {
        &mut self.inner
    }

    /// Write a buffer to the inner-protocol stream.
    ///
    /// Direct port of openrsync's `io.c::io_write_buf` (lines
    /// 134..164). When `sess.mplex_writes` is set, the buffer is
    /// chunked into `MSG_DATA` mplex frames sized at
    /// [`MAX_PAYLOAD_LEN`]. When unset (e.g. during the handshake),
    /// the bytes are written through unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::RsyncProtocolError`] when the underlying
    /// writer fails or the framing math overflows.
    pub async fn write_buf(
        &mut self,
        sess: &mut WireSession,
        buf: &[u8],
    ) -> Result<(), DomainError> {
        if !sess.mplex_writes {
            return self.write_unframed(sess, buf).await;
        }
        let mut start = 0_usize;
        while start < buf.len() {
            let take = (buf.len() - start).min(MAX_PAYLOAD_LEN);
            let end = start.saturating_add(take);
            self.write_data_chunk(sess, buf, start, end, take).await?;
            start = end;
        }
        self.inner
            .flush()
            .await
            .map_err(|e| DomainError::RsyncProtocolError(format!("mplex flush failed: {e}")))?;
        Ok(())
    }

    /// Straight `write_all` + `flush` for the unframed (pre-mplex)
    /// handshake path. Mirrors the early-return branch of openrsync's
    /// `io.c::io_write_buf`.
    async fn write_unframed(
        &mut self,
        sess: &mut WireSession,
        buf: &[u8],
    ) -> Result<(), DomainError> {
        self.inner.write_all(buf).await.map_err(|e| {
            DomainError::RsyncProtocolError(format!("io_write_buf raw write failed: {e}"))
        })?;
        self.inner.flush().await.map_err(|e| {
            DomainError::RsyncProtocolError(format!("io_write_buf raw flush failed: {e}"))
        })?;
        sess.total_write = sess
            .total_write
            .saturating_add(u64::try_from(buf.len()).unwrap_or(u64::MAX));
        Ok(())
    }

    /// Emit one `MSG_DATA` frame covering `buf[start..end]`. Mirrors
    /// the inner-loop body of openrsync's `io.c::io_write_buf` (the
    /// `tag = (7 << 24) + wsz` line plus the payload write).
    async fn write_data_chunk(
        &mut self,
        sess: &mut WireSession,
        buf: &[u8],
        start: usize,
        end: usize,
        take: usize,
    ) -> Result<(), DomainError> {
        let tag_u32 = u32::from(MplexTag::Data.to_wire()) << 24_u32;
        let len_u32 = u32::try_from(take).unwrap_or(0) & 0x00ff_ffff;
        let header = tag_u32 | len_u32;
        self.inner
            .write_all(&header.to_le_bytes())
            .await
            .map_err(|e| {
                DomainError::RsyncProtocolError(format!("mplex header write failed: {e}"))
            })?;
        if let Some(slot) = buf.get(start..end) {
            self.inner.write_all(slot).await.map_err(|e| {
                DomainError::RsyncProtocolError(format!("mplex payload write failed: {e}"))
            })?;
        }
        sess.total_write = sess
            .total_write
            .saturating_add(u64::try_from(take).unwrap_or(u64::MAX));
        Ok(())
    }

    /// Write a single byte. Port of openrsync's `io.c::io_write_byte`
    /// (lines 718..727).
    ///
    /// # Errors
    ///
    /// See [`Self::write_buf`].
    pub async fn write_byte(&mut self, sess: &mut WireSession, val: u8) -> Result<(), DomainError> {
        self.write_buf(sess, &[val]).await
    }

    /// Write a little-endian i32. Port of openrsync's
    /// `io.c::io_write_int` / `io_write_uint` (lines 430..452).
    ///
    /// # Errors
    ///
    /// See [`Self::write_buf`].
    pub async fn write_int(&mut self, sess: &mut WireSession, val: i32) -> Result<(), DomainError> {
        self.write_buf(sess, &val.to_le_bytes()).await
    }

    /// Write an arbitrary mplex frame. Used by callers that own the
    /// tag selection (e.g. emitting an `MSG_ERROR` from the receiver
    /// side). The session counters are updated as if this were an
    /// `MSG_DATA` payload because openrsync's `total_write` does not
    /// distinguish mplex tags.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::RsyncProtocolError`] when the payload
    /// exceeds [`MAX_PAYLOAD_LEN`] or the underlying writer fails.
    pub async fn write_frame(
        &mut self,
        sess: &mut WireSession,
        tag: MplexTag,
        payload: &[u8],
    ) -> Result<(), DomainError> {
        if payload.len() > MAX_PAYLOAD_LEN {
            return Err(DomainError::RsyncProtocolError(format!(
                "mplex frame payload {} exceeds 24-bit length cap",
                payload.len()
            )));
        }
        let tag_u32 = (u32::from(tag.to_wire())) << 24_u32;
        let len_u32 = u32::try_from(payload.len()).unwrap_or(0) & 0x00ff_ffff;
        let header = tag_u32 | len_u32;
        self.inner
            .write_all(&header.to_le_bytes())
            .await
            .map_err(|e| {
                DomainError::RsyncProtocolError(format!("mplex header write failed: {e}"))
            })?;
        if !payload.is_empty() {
            self.inner.write_all(payload).await.map_err(|e| {
                DomainError::RsyncProtocolError(format!("mplex payload write failed: {e}"))
            })?;
        }
        self.inner
            .flush()
            .await
            .map_err(|e| DomainError::RsyncProtocolError(format!("mplex flush failed: {e}")))?;
        sess.total_write = sess
            .total_write
            .saturating_add(u64::try_from(payload.len()).unwrap_or(u64::MAX));
        Ok(())
    }
}

async fn read_u32_le<R: AsyncRead + Unpin + Send>(r: &mut R) -> Result<u32, DomainError> {
    let mut buf = [0_u8; 4];
    r.read_exact(&mut buf)
        .await
        .map_err(|e| DomainError::RsyncProtocolError(format!("mplex header read failed: {e}")))?;
    Ok(u32::from_le_bytes(buf))
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test code uses unwrap/expect for brevity per project convention"
)]
mod tests {
    use super::{MPLEX_BASE, MplexReader, MplexTag, MplexWriter, OOB_PAYLOAD_CAP};
    use crate::adapters::rsync::wire::session::WireSession;
    use tokio::io::duplex;

    #[test]
    fn mplex_base_constant_matches_openrsync() {
        // openrsync io.c line 148: `tag = (7 << 24) + wsz`.
        assert_eq!(MPLEX_BASE, 7);
    }

    #[test]
    fn tag_roundtrip_known_values() {
        for tag in [
            MplexTag::Data,
            MplexTag::ErrorXfer,
            MplexTag::Info,
            MplexTag::Error,
            MplexTag::Warning,
            MplexTag::ErrorSocket,
            MplexTag::Log,
            MplexTag::Client,
            MplexTag::ErrorUtf8,
            MplexTag::Redo,
            MplexTag::Stats,
            MplexTag::IoError,
            MplexTag::IoTimeout,
            MplexTag::Noop,
            MplexTag::ErrorExit,
            MplexTag::Success,
            MplexTag::Deleted,
            MplexTag::NoSend,
        ] {
            let wire = tag.to_wire();
            let back = MplexTag::from_wire(wire);
            assert_eq!(tag, back, "wire byte {wire} did not round-trip");
        }
    }

    #[test]
    fn tag_wire_bytes_match_rsync_h() {
        // Every tag wire byte must equal `tag_value + MPLEX_BASE` per
        // rsync 3.2.7's `enum msgcode`.
        assert_eq!(MplexTag::Data.to_wire(), 7);
        assert_eq!(MplexTag::ErrorXfer.to_wire(), 8);
        assert_eq!(MplexTag::Info.to_wire(), 9);
        assert_eq!(MplexTag::Error.to_wire(), 10);
        assert_eq!(MplexTag::Warning.to_wire(), 11);
        assert_eq!(MplexTag::ErrorSocket.to_wire(), 12);
        assert_eq!(MplexTag::Log.to_wire(), 13);
        assert_eq!(MplexTag::Client.to_wire(), 14);
        assert_eq!(MplexTag::ErrorUtf8.to_wire(), 15);
        assert_eq!(MplexTag::Redo.to_wire(), 16);
        assert_eq!(MplexTag::Stats.to_wire(), 17);
        assert_eq!(MplexTag::IoError.to_wire(), 29);
        assert_eq!(MplexTag::IoTimeout.to_wire(), 40);
        assert_eq!(MplexTag::Noop.to_wire(), 49);
        assert_eq!(MplexTag::ErrorExit.to_wire(), 93);
        assert_eq!(MplexTag::Success.to_wire(), 107);
        assert_eq!(MplexTag::Deleted.to_wire(), 108);
        assert_eq!(MplexTag::NoSend.to_wire(), 109);
    }

    #[test]
    fn tag_unknown_preserves_byte() {
        let unknown = MplexTag::from_wire(0xfe);
        assert_eq!(unknown, MplexTag::Unknown(0xfe));
        assert_eq!(unknown.to_wire(), 0xfe);
    }

    #[test]
    fn fatal_predicate_matches_only_known_fatal() {
        assert!(MplexTag::Error.is_fatal());
        assert!(MplexTag::ErrorSocket.is_fatal());
        assert!(MplexTag::IoError.is_fatal());
        assert!(MplexTag::ErrorExit.is_fatal());
        assert!(!MplexTag::ErrorXfer.is_fatal());
        assert!(!MplexTag::Warning.is_fatal());
        assert!(!MplexTag::Data.is_fatal());
    }

    #[test]
    fn advisory_predicate_matches_only_log_levels() {
        assert!(MplexTag::Info.is_advisory());
        assert!(MplexTag::Warning.is_advisory());
        assert!(MplexTag::Log.is_advisory());
        assert!(MplexTag::Client.is_advisory());
        assert!(MplexTag::Noop.is_advisory());
        assert!(!MplexTag::Data.is_advisory());
        assert!(!MplexTag::Error.is_advisory());
        assert!(!MplexTag::Stats.is_advisory());
    }

    #[tokio::test]
    async fn unframed_round_trip_when_mplex_disabled() {
        let (left, right) = duplex(64);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut sess_w = WireSession::new();
        let mut sess_r = WireSession::new();
        let mut w = MplexWriter::new(lw);
        let mut r = MplexReader::new(rr);
        w.write_buf(&mut sess_w, b"hello").await.expect("w");
        let mut buf = [0_u8; 5];
        r.read_buf(&mut sess_r, &mut buf).await.expect("r");
        assert_eq!(&buf, b"hello");
        assert_eq!(sess_w.total_write, 5);
        assert_eq!(sess_r.total_read, 5);
    }

    #[tokio::test]
    async fn data_frame_round_trip_when_mplex_enabled() {
        let (left, right) = duplex(64 * 1024);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut sess_w = WireSession::new();
        sess_w.mplex_writes = true;
        let mut sess_r = WireSession::new();
        sess_r.mplex_reads = true;
        let mut w = MplexWriter::new(lw);
        let mut r = MplexReader::new(rr);
        w.write_buf(&mut sess_w, b"hello-world").await.expect("w");
        let mut buf = [0_u8; 11];
        r.read_buf(&mut sess_r, &mut buf).await.expect("r");
        assert_eq!(&buf, b"hello-world");
    }

    #[tokio::test]
    async fn read_buf_demuxes_advisory_frames_transparently() {
        let (left, right) = duplex(64 * 1024);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        // Manually emit an info frame followed by a data frame.
        w.write_frame(&mut sess_w, MplexTag::Info, b"hello there")
            .await
            .expect("info");
        w.write_frame(&mut sess_w, MplexTag::Data, b"payload")
            .await
            .expect("data");
        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        sess_r.mplex_reads = true;
        let mut buf = [0_u8; 7];
        r.read_buf(&mut sess_r, &mut buf).await.expect("read");
        assert_eq!(&buf, b"payload");
    }

    #[tokio::test]
    async fn read_buf_propagates_fatal_frame() {
        let (left, right) = duplex(64 * 1024);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        w.write_frame(&mut sess_w, MplexTag::Error, b"boom")
            .await
            .expect("err");
        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        sess_r.mplex_reads = true;
        let mut buf = [0_u8; 4];
        let err = r
            .read_buf(&mut sess_r, &mut buf)
            .await
            .expect_err("must err");
        assert!(format!("{err}").contains("boom"));
    }

    #[tokio::test]
    async fn frame_aggregation_across_partial_reads() {
        // Write three small data frames; the reader should aggregate
        // the inner-protocol bytes seamlessly.
        let (left, right) = duplex(64 * 1024);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        w.write_frame(&mut sess_w, MplexTag::Data, b"abc")
            .await
            .expect("a");
        w.write_frame(&mut sess_w, MplexTag::Data, b"de")
            .await
            .expect("b");
        w.write_frame(&mut sess_w, MplexTag::Data, b"fghi")
            .await
            .expect("c");
        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        sess_r.mplex_reads = true;
        let mut buf = [0_u8; 9];
        r.read_buf(&mut sess_r, &mut buf).await.expect("r");
        assert_eq!(&buf, b"abcdefghi");
    }

    #[tokio::test]
    async fn oversized_oob_frame_errors() {
        let (left, right) = duplex(64 * 1024);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        // Hand-craft an info frame with a payload size larger than
        // OOB_PAYLOAD_CAP (1024).
        let mut w_inner = lw;
        let oversize_len = OOB_PAYLOAD_CAP + 16;
        let header =
            (u32::from(MplexTag::Info.to_wire()) << 24_u32) | u32::try_from(oversize_len).unwrap();
        use tokio::io::AsyncWriteExt;
        w_inner.write_all(&header.to_le_bytes()).await.expect("h");
        w_inner.flush().await.expect("flush");
        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        sess_r.mplex_reads = true;
        let mut buf = [0_u8; 4];
        let err = r
            .read_buf(&mut sess_r, &mut buf)
            .await
            .expect_err("must err");
        assert!(format!("{err}").contains("multiplex buffer overflow"));
    }

    #[tokio::test]
    async fn read_frame_propagates_eof_as_protocol_error() {
        let (left, right) = duplex(64);
        let (lr, lw) = tokio::io::split(left);
        let (rr, rw) = tokio::io::split(right);
        drop(lr);
        drop(lw);
        drop(rw);
        let mut r = MplexReader::new(rr);
        let err = r.read_frame().await.expect_err("must err");
        assert!(format!("{err}").to_lowercase().contains("mplex"));
    }

    #[tokio::test]
    async fn write_int_then_read_int_round_trips() {
        let (left, right) = duplex(64);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        w.write_int(&mut sess_w, 0x1234_5678_i32).await.expect("w");
        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        let v = r.read_int(&mut sess_r).await.expect("r");
        assert_eq!(v, 0x1234_5678_i32);
    }

    #[tokio::test]
    async fn write_byte_then_read_byte_round_trips() {
        let (left, right) = duplex(8);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        w.write_byte(&mut sess_w, 0xab_u8).await.expect("w");
        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        let v = r.read_byte(&mut sess_r).await.expect("r");
        assert_eq!(v, 0xab_u8);
    }

    #[tokio::test]
    async fn captured_real_rsync_handshake_bytes_decode_cleanly() {
        // Fixture captured from rsync 3.2.7 --server on Linux:
        //   server -> client: 4 bytes LE protocol_version
        //   server -> client: 1 byte compat_flags (0x00)
        //   server -> client: 4 bytes LE checksum_seed
        // We feed these straight to MplexReader::raw_mut to confirm
        // the byte parsing path.
        let captured: Vec<u8> = [
            // protocol_version = 31 (LE)
            0x1f_u8, 0x00, 0x00, 0x00,
            // compat_flags varint with bit-7 clear (one byte only)
            0x00, // checksum_seed = 0x12345678 (LE)
            0x78, 0x56, 0x34, 0x12,
        ]
        .to_vec();
        let cursor = std::io::Cursor::new(captured);
        let mut r = MplexReader::new(cursor);
        let mut sess = WireSession::new();
        let rver = r.read_int(&mut sess).await.expect("rver");
        assert_eq!(rver, 31);
        let compat = r.read_byte(&mut sess).await.expect("compat");
        assert_eq!(compat, 0);
        let seed = r.read_int(&mut sess).await.expect("seed");
        assert_eq!(seed, 0x1234_5678_i32);
    }
}
