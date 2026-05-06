// SPDX-License-Identifier: ISC
//! File-list index codec — port of upstream rsync 3.2.7's
//! `io.c::read_ndx` / `write_ndx` (lines 2240..2330).
//!
//! Original copyright: Andrew Tridgell, Wayne Davison, and others;
//! GPL. The encoder + decoder math here is a direct translation of
//! the upstream implementation; the bit-shuffle is identical.
//!
//! # Why a separate module
//!
//! At protocol >= 30 the sender / receiver exchange file-list indices
//! via a stateful byte-reduction codec rather than plain `write_int` /
//! `read_int`. The codec keeps two `prev_*` cursors per direction that
//! are mutated on every successful read or write — a classic "rolling
//! diff" compression that buys ~3x bandwidth reduction for the typical
//! ascending-index access pattern.
//!
//! `NDX_DONE = -1` (the "phase-done" sentinel) collapses to a single
//! `0x00` byte, which is why the slice-3 sender state machine that
//! used `write_int(-1)` (4 bytes LE) was producing wire bytes the
//! server interpreted as a positive 4-byte sentinel. Slice 4 uses the
//! codec verbatim against rsync 3.2.7.
//!
//! # Lock-free contract
//!
//! [`NdxState`] is owned by the per-direction task and threaded as
//! `&mut`. Never wrapped in `Arc<Mutex<...>>`. Reader and writer ends
//! own their own [`NdxState`] — there is no cross-task sharing.

use tokio::io::{AsyncRead, AsyncWrite};

use crate::adapters::rsync::wire::io::{MplexReader, MplexWriter};
use crate::adapters::rsync::wire::session::WireSession;
use crate::domain::error::DomainError;

/// "Phase done" sentinel. Mirrors upstream rsync's `rsync.h` line 284
/// `#define NDX_DONE -1`. On the wire (protocol >= 30) it collapses to
/// a single `0x00` byte.
pub const NDX_DONE: i32 = -1;

/// Per-direction codec state.
///
/// Mirrors the `static int32 prev_positive = -1, prev_negative = 1`
/// initialisation in upstream rsync's `io.c::read_ndx` /
/// `write_ndx`. Each direction keeps its own state — reader and
/// writer never share a [`NdxState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NdxState {
    /// Last positive index seen on the wire, used as the diff base
    /// for the next positive index.
    pub prev_positive: i32,
    /// Last negative index seen on the wire, used as the diff base
    /// for the next negative index.
    pub prev_negative: i32,
}

impl NdxState {
    /// Build a fresh codec state — `prev_positive = -1`,
    /// `prev_negative = 1`. Mirrors upstream rsync's static-variable
    /// initialisation.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            prev_positive: -1,
            prev_negative: 1,
        }
    }
}

impl Default for NdxState {
    fn default() -> Self {
        Self::new()
    }
}

/// Read one file-list index off the wire.
///
/// Direct port of upstream rsync 3.2.7's `io.c::read_ndx` (lines
/// 2289..2320). At protocol < 30 the codec degrades to a plain
/// `read_int`. At protocol >= 30 the bit-shuffle is used.
///
/// # Errors
///
/// Returns [`DomainError::RsyncProtocolError`] on transport failure or
/// when the inner I/O surfaces an error.
pub async fn read_ndx<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    state: &mut NdxState,
    negotiated: i32,
) -> Result<i32, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    if negotiated < 30 {
        return reader.read_int(sess).await;
    }
    let first = reader.read_byte(sess).await?;
    if first == 0 {
        return Ok(NDX_DONE);
    }
    let (head, is_negative) = if first == 0xff {
        let next = reader.read_byte(sess).await?;
        (next, true)
    } else {
        (first, false)
    };
    let num = decode_ndx_payload(reader, sess, state, head, is_negative).await?;
    Ok(commit_ndx_state(state, num, is_negative))
}

/// Decode the index payload after the prefix byte(s) have been pulled
/// off the wire. Splits short-form (single byte → diff) from long-form
/// (`0xFE` prefix → 2- or 4-byte tail) to keep `read_ndx` short.
async fn decode_ndx_payload<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    state: &NdxState,
    head: u8,
    is_negative: bool,
) -> Result<i32, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    if head == 0xfe {
        return read_long_ndx(reader, sess, state, is_negative).await;
    }
    let prev = if is_negative {
        state.prev_negative
    } else {
        state.prev_positive
    };
    Ok(i32::from(head).saturating_add(prev))
}

/// Commit the freshly-decoded value into the appropriate `prev_*`
/// cursor and return the externally-visible signed index.
fn commit_ndx_state(state: &mut NdxState, num: i32, is_negative: bool) -> i32 {
    if is_negative {
        state.prev_negative = num;
        num.checked_neg().unwrap_or(i32::MIN)
    } else {
        state.prev_positive = num;
        num
    }
}

/// Decode the long-form (`0xFE` prefix) variant. Mirrors the
/// `if (CVAL(b, 0) == 0xFE)` branch of upstream `read_ndx`.
async fn read_long_ndx<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    state: &NdxState,
    is_negative: bool,
) -> Result<i32, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    let mut two = [0_u8; 2];
    reader.read_buf(sess, &mut two).await?;
    let prev = if is_negative {
        state.prev_negative
    } else {
        state.prev_positive
    };
    if (two[0] & 0x80) != 0 {
        let mut tail = [0_u8; 2];
        reader.read_buf(sess, &mut tail).await?;
        let bytes = [two[1], tail[0], tail[1], two[0] & 0x7f];
        return Ok(i32::from_le_bytes(bytes));
    }
    let combined = (i32::from(two[0]) << 8_u32).saturating_add(i32::from(two[1]));
    Ok(combined.saturating_add(prev))
}

/// Write one file-list index to the wire.
///
/// Direct port of upstream rsync 3.2.7's `io.c::write_ndx` (lines
/// 2240..2287). At protocol < 30 the codec degrades to a plain
/// `write_int`. At protocol >= 30 the bit-shuffle is used.
///
/// # Errors
///
/// Returns [`DomainError::RsyncProtocolError`] on transport failure.
pub async fn write_ndx<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    state: &mut NdxState,
    negotiated: i32,
    ndx: i32,
) -> Result<(), DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    if negotiated < 30 {
        return writer.write_int(sess, ndx).await;
    }
    if ndx == NDX_DONE {
        return writer.write_byte(sess, 0).await;
    }
    let mut buf = [0_u8; 6];
    let mut cnt = 0_usize;
    let (positive, prev) = if ndx >= 0 {
        let prev = state.prev_positive;
        state.prev_positive = ndx;
        (ndx, prev)
    } else {
        buf[cnt] = 0xff;
        cnt = cnt.saturating_add(1);
        let abs = ndx.checked_neg().unwrap_or(i32::MAX);
        let prev = state.prev_negative;
        state.prev_negative = abs;
        (abs, prev)
    };
    cnt = encode_diff(&mut buf, cnt, positive, prev);
    let payload = buf
        .get(..cnt)
        .ok_or_else(|| DomainError::RsyncProtocolError("ndx: write OOB".to_string()))?;
    writer.write_buf(sess, payload).await
}

/// Compose the diff bytes per `write_ndx`'s body. Pure arithmetic —
/// kept separate from [`write_ndx`] so the async fn stays under the
/// 30-line cognitive-complexity threshold.
fn encode_diff(buf: &mut [u8; 6], cnt: usize, ndx: i32, prev: i32) -> usize {
    let diff = ndx.saturating_sub(prev);
    if (1..0xfe).contains(&diff) {
        return write_short_diff(buf, cnt, diff);
    }
    if !(0..=0x7fff).contains(&diff) {
        return write_long_diff(buf, cnt, ndx);
    }
    write_medium_diff(buf, cnt, diff)
}

/// Emit the 1-byte short-form diff (`diff` ∈ `1..0xfe`).
fn write_short_diff(buf: &mut [u8; 6], cnt: usize, diff: i32) -> usize {
    let byte = u8::try_from(diff & 0xff).unwrap_or(0);
    if let Some(slot) = buf.get_mut(cnt) {
        *slot = byte;
    }
    cnt.saturating_add(1)
}

/// Emit the 3-byte medium-form diff (`0xFE` prefix + 2-byte big-endian
/// diff). Mirrors the upstream `else` branch in `write_ndx`.
fn write_medium_diff(buf: &mut [u8; 6], cnt: usize, diff: i32) -> usize {
    let mut idx = cnt;
    if let Some(slot) = buf.get_mut(idx) {
        *slot = 0xfe;
    }
    idx = idx.saturating_add(1);
    let high_byte = u8::try_from((diff >> 8_u32) & 0xff).unwrap_or(0);
    let low_byte = u8::try_from(diff & 0xff).unwrap_or(0);
    if let Some(slot) = buf.get_mut(idx) {
        *slot = high_byte;
    }
    idx = idx.saturating_add(1);
    if let Some(slot) = buf.get_mut(idx) {
        *slot = low_byte;
    }
    idx.saturating_add(1)
}

/// Emit the 5-byte long-form (`0xFE` prefix + 4-byte payload with
/// high-bit-set on the first byte). Mirrors the upstream `if (CVAL(b,
/// 0) & 0x80)` branch — the diff is too large to fit in 15 bits, so
/// we send the absolute value of `ndx` directly with a marker bit.
fn write_long_diff(buf: &mut [u8; 6], cnt: usize, ndx: i32) -> usize {
    let mut idx = cnt;
    if let Some(slot) = buf.get_mut(idx) {
        *slot = 0xfe;
    }
    idx = idx.saturating_add(1);
    let bytes = ndx.to_le_bytes();
    let high_with_flag = bytes[3] | 0x80;
    let composed = [high_with_flag, bytes[0], bytes[1], bytes[2]];
    for byte in composed {
        if let Some(slot) = buf.get_mut(idx) {
            *slot = byte;
        }
        idx = idx.saturating_add(1);
    }
    idx
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test code uses unwrap/expect for brevity per project convention"
)]
mod tests {
    use super::{NDX_DONE, NdxState, read_ndx, write_ndx};
    use crate::adapters::rsync::wire::io::{MplexReader, MplexWriter};
    use crate::adapters::rsync::wire::session::WireSession;
    use tokio::io::duplex;

    #[test]
    fn ndx_state_default_matches_upstream_initialisation() {
        let s = NdxState::new();
        assert_eq!(s.prev_positive, -1);
        assert_eq!(s.prev_negative, 1);
    }

    #[tokio::test]
    async fn ndx_done_round_trips_as_single_zero_byte() {
        let (left, right) = duplex(8);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        let mut state_w = NdxState::new();
        write_ndx(&mut w, &mut sess_w, &mut state_w, 31, NDX_DONE)
            .await
            .expect("write");
        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        let mut state_r = NdxState::new();
        let got = read_ndx(&mut r, &mut sess_r, &mut state_r, 31)
            .await
            .expect("read");
        assert_eq!(got, NDX_DONE);
    }

    #[tokio::test]
    async fn ndx_first_positive_index_round_trips_through_diff() {
        // First positive idx after init: prev_positive=-1, diff=ndx+1.
        // Send 0 → diff=1 → 1-byte encoding [0x01].
        let (left, right) = duplex(8);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        let mut state_w = NdxState::new();
        write_ndx(&mut w, &mut sess_w, &mut state_w, 31, 0)
            .await
            .expect("write");
        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        let mut state_r = NdxState::new();
        let got = read_ndx(&mut r, &mut sess_r, &mut state_r, 31)
            .await
            .expect("read");
        assert_eq!(got, 0);
        assert_eq!(state_r.prev_positive, 0);
    }

    #[tokio::test]
    async fn ndx_ascending_positive_sequence_round_trips() {
        let (left, right) = duplex(64);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        let mut state_w = NdxState::new();
        for ndx in [0_i32, 1, 5, 10, 100, 200] {
            write_ndx(&mut w, &mut sess_w, &mut state_w, 31, ndx)
                .await
                .expect("write");
        }
        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        let mut state_r = NdxState::new();
        for expected in [0_i32, 1, 5, 10, 100, 200] {
            let got = read_ndx(&mut r, &mut sess_r, &mut state_r, 31)
                .await
                .expect("read");
            assert_eq!(got, expected, "expected {expected}");
        }
    }

    #[tokio::test]
    async fn ndx_proto_below_30_falls_back_to_plain_read_int() {
        let (left, right) = duplex(16);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        let mut state_w = NdxState::new();
        write_ndx(&mut w, &mut sess_w, &mut state_w, 27, 0x1234_5678)
            .await
            .expect("write");
        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        let mut state_r = NdxState::new();
        let got = read_ndx(&mut r, &mut sess_r, &mut state_r, 27)
            .await
            .expect("read");
        assert_eq!(got, 0x1234_5678);
    }
}
