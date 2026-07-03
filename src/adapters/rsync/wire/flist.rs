// SPDX-License-Identifier: ISC
//! Ported from OpenBSD's openrsync — `flist.c`.
//!
//! Original copyright: Kristaps Dzonsons; ISC license. See
//! `LICENSES/openrsync-ISC.txt` for the full notice.
//!
//! This port maintains openrsync's struct names + field order to ease
//! cross-references against the C source. The I/O layer is rewritten to
//! async + lock-free Rust per the project's hot-path invariants.
//!
//! # Slice 2 scope (v7.0.0-alpha.8)
//!
//! - [`Flist`] — value-object mirror of openrsync's `struct flist` +
//!   `struct flstat` (collapsed because the Rust type is value-only and
//!   doesn't benefit from the C nesting).
//! - [`recv_flist`] — port of `flist.c::flist_recv` (lines 597..795). The
//!   peer's file list is drained off an [`MplexReader`] using the
//!   `FLIST_*` flag matrix (see below). Returns the entries + the two
//!   identifier sub-lists that openrsync streams alongside the flist.
//! - [`send_flist`] — port of `flist.c::flist_send` (lines 264..428).
//!   Writes the local list onto an [`MplexWriter`]. As openrsync does,
//!   we always pin `FLIST_NAME_LONG` on every entry — that produces a
//!   degenerate but valid flag combination the rsync 3.2.x server
//!   parses identically to the more compact upstream encoding.
//! - [`gen_flist_local`] — minimal walk of a local directory tree
//!   producing a sorted [`Vec<Flist>`]. Match-pattern filtering ships in
//!   slice 3 alongside the sender state machine.
//!
//! # Wire flags — `FLIST_*` vs upstream `XMIT_*`
//!
//! openrsync targets the protocol-27 wire shape and uses an 8-bit
//! `FLIST_*` flag set defined inside `flist.c` (lines 55..62):
//!
//! | Bit    | openrsync name      | Upstream rsync name (3.2.x)        |
//! |--------|---------------------|------------------------------------|
//! | 0x0001 | `FLIST_TOP_LEVEL`   | `XMIT_TOP_DIR`                     |
//! | 0x0002 | `FLIST_MODE_SAME`   | `XMIT_SAME_MODE`                   |
//! | 0x0004 | `FLIST_RDEV_SAME`   | (no direct equivalent; see note)   |
//! | 0x0008 | `FLIST_UID_SAME`    | `XMIT_SAME_UID`                    |
//! | 0x0010 | `FLIST_GID_SAME`    | `XMIT_SAME_GID`                    |
//! | 0x0020 | `FLIST_NAME_SAME`   | `XMIT_SAME_NAME`                   |
//! | 0x0040 | `FLIST_NAME_LONG`   | `XMIT_LONG_NAME`                   |
//! | 0x0080 | `FLIST_TIME_SAME`   | `XMIT_SAME_TIME`                   |
//!
//! Slice-2 follows openrsync verbatim and carries the `FLIST_*` names
//! per the project's "match openrsync's struct names + field order"
//! rule. The upstream `XMIT_EXTENDED_FLAGS` two-byte header (used at
//! protocols >= 28) is **not** part of the port: openrsync 27 has no
//! concept of it and the rsync server tolerates the 8-bit shape when
//! the client never sets `XMIT_EXTENDED_FLAGS` first.
//!
//! # Lock-free contract (CRITICAL)
//!
//! All state lives on the per-session task's stack. The recv/send
//! drivers thread `&mut WireSession` through every I/O call and own
//! their respective reader / writer halves exclusively. No `Mutex`
//! anywhere on the path; the openrsync `LOG3` global state has no
//! Rust counterpart — diagnostics travel through `tracing` instead.

use std::fs::Metadata;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use globset::GlobSet;
use tokio::fs::{self, DirEntry};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::adapters::rsync::types::PreserveFlags;
use crate::adapters::rsync::wire::io::{MplexReader, MplexWriter};
use crate::adapters::rsync::wire::session::{
    VARINT_FLIST_MIN_PROTOCOL, WireSession, XMIT_EXTENDED_FLAGS_MIN_PROTOCOL,
};
use crate::domain::error::DomainError;

// =====================================================================
// FLIST_* flag matrix
// =====================================================================

/// Top-level directory marker — needed for remote `--delete`. Mirrors
/// openrsync's `FLIST_TOP_LEVEL` (0x0001) and upstream rsync's
/// `XMIT_TOP_DIR`.
pub const FLIST_TOP_LEVEL: u8 = 0x01;

/// Mode bits identical to the previous entry. Mirrors openrsync's
/// `FLIST_MODE_SAME` (0x0002) and upstream rsync's `XMIT_SAME_MODE`.
pub const FLIST_MODE_SAME: u8 = 0x02;

/// `rdev` identical to the previous entry. Mirrors openrsync's
/// `FLIST_RDEV_SAME` (0x0004). Upstream rsync uses a different bit
/// pattern at protocol 30+ — see module-level docs.
pub const FLIST_RDEV_SAME: u8 = 0x04;

/// `uid` identical to the previous entry. Mirrors openrsync's
/// `FLIST_UID_SAME` (0x0008) and upstream rsync's `XMIT_SAME_UID`.
pub const FLIST_UID_SAME: u8 = 0x08;

/// `gid` identical to the previous entry. Mirrors openrsync's
/// `FLIST_GID_SAME` (0x0010) and upstream rsync's `XMIT_SAME_GID`.
pub const FLIST_GID_SAME: u8 = 0x10;

/// Filename shares a leading prefix with the previous entry. Mirrors
/// openrsync's `FLIST_NAME_SAME` (0x0020) and upstream rsync's
/// `XMIT_SAME_NAME`.
pub const FLIST_NAME_SAME: u8 = 0x20;

/// Filename remainder length is encoded as 4 bytes (LE i32) instead of one byte.
/// Mirrors openrsync's `FLIST_NAME_LONG` (0x0040) and upstream rsync's `XMIT_LONG_NAME`.
pub const FLIST_NAME_LONG: u8 = 0x40;

/// `mtime` identical to the previous entry. Mirrors openrsync's
/// `FLIST_TIME_SAME` (0x0080) and upstream rsync's `XMIT_SAME_TIME`.
pub const FLIST_TIME_SAME: u8 = 0x80;

// =====================================================================
// XMIT_EXTENDED_FLAGS — protocol 28+ wire shape
// =====================================================================

/// Marker bit set on the low byte of the flag field at protocol 28+.
/// When the receiver sees this bit, it pulls a second byte off the
/// wire and OR-shifts it `<< 8` into `flags`. Mirrors upstream rsync's
/// `XMIT_EXTENDED_FLAGS` (`1 << 2 = 0x04`). At protocol 27 the same
/// bit was `XMIT_SAME_RDEV_pre28`; the 16-bit shape is gated on
/// `negotiated >= 28` (see [`XMIT_EXTENDED_FLAGS_MIN_PROTOCOL`]).
const XMIT_EXTENDED_FLAGS: u16 = 1 << 2;

/// Top-level directory marker as a 16-bit value. Mirrors `XMIT_TOP_DIR`
/// (`1 << 0`). Same numeric value as [`FLIST_TOP_LEVEL`].
const XMIT_TOP_DIR_16: u16 = 1 << 0;

/// 16-bit `FLIST_NAME_LONG` mirror — `XMIT_LONG_NAME` (`1 << 6`).
const XMIT_LONG_NAME_16: u16 = 1 << 6;

/// Hard-link present (proto 28+, non-dir entries). When the
/// companion [`XMIT_HLINK_FIRST`] bit is *unset*, the entry is the
/// non-first member of a hardlink set and the receiver pulls one
/// `read_varint` off the wire holding the leader's flist index.
/// Mirrors upstream rsync's `XMIT_HLINKED` (`1 << 9`).
const XMIT_HLINKED: u16 = 1 << 9;

/// Username-string follows the uid (proto 30+, when `--owner` is on
/// AND `inc_recurse` is on AND the user has a name). Mirrors upstream
/// rsync's `XMIT_USER_NAME_FOLLOWS` (`1 << 10`).
const XMIT_USER_NAME_FOLLOWS: u16 = 1 << 10;

/// Groupname-string follows the gid (proto 30+, when `--group` is on
/// AND `inc_recurse` is on AND the group has a name). Mirrors upstream
/// rsync's `XMIT_GROUP_NAME_FOLLOWS` (`1 << 11`).
const XMIT_GROUP_NAME_FOLLOWS: u16 = 1 << 11;

/// First member of a hardlink set (proto 30+; only set when
/// [`XMIT_HLINKED`] is set on a non-dir entry). The leader carries the
/// full attribute set; followers reuse them by referencing the leader's
/// flist index via [`XMIT_HLINKED`] without [`XMIT_HLINK_FIRST`].
/// Mirrors upstream rsync's `XMIT_HLINK_FIRST` (`1 << 12`).
const XMIT_HLINK_FIRST: u16 = 1 << 12;

/// End-of-list marker carrying an `io_error` count (proto 31+, encoded
/// as the *first* byte being 0 followed by a 16-bit flag short whose
/// payload is `XMIT_EXTENDED_FLAGS | XMIT_IO_ERROR_ENDLIST`). When the
/// receiver hits this combination it pulls a `read_varint` off the
/// wire holding the sender's accumulated `io_error` count and breaks
/// out of the receive loop. Numerically `1 << 12` — same bit pattern
/// as [`XMIT_HLINK_FIRST`], but only meaningful on the end-of-list
/// frame where [`XMIT_EXTENDED_FLAGS`] is the only other bit set.
/// Mirrors upstream rsync's `XMIT_IO_ERROR_ENDLIST` (`1 << 12`).
const XMIT_IO_ERROR_ENDLIST: u16 = 1 << 12;

/// Modification-time nanoseconds suffix (proto 31+). When set, an
/// extra `read_varint` follows the mtime carrying the sub-second
/// nanoseconds. We discard the value — the `Flist` value-object only
/// preserves whole-second mtimes. Mirrors upstream rsync's
/// `XMIT_MOD_NSEC` (`1 << 13`).
const XMIT_MOD_NSEC: u16 = 1 << 13;

// =====================================================================
// varint / varlong codecs — protocol 30+ wire shape
// =====================================================================

/// Lookup table mapping `byte / 4` to the count of follow-up bytes for
/// the varint / varlong codecs. Mirrors `int_byte_extra[64]` in upstream
/// rsync 3.2.7's `io.c` line 119. The table is dense (64 entries) and
/// reads `int_byte_extra[byte / 4]` per `read_varint` / `read_varlong`.
const INT_BYTE_EXTRA: [u8; 64] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // (00 - 3F)/4
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // (40 - 7F)/4
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, // (80 - BF)/4
    2, 2, 2, 2, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 6, // (C0 - FF)/4
];

// =====================================================================
// POSIX mode-bit predicates
// =====================================================================

/// POSIX file-type mask. Same value as `S_IFMT` on every Unix.
const S_IFMT: u32 = 0o170_000;
/// POSIX regular-file marker. Same value as `S_IFREG`.
const S_IFREG: u32 = 0o100_000;
/// POSIX directory marker. Same value as `S_IFDIR`.
const S_IFDIR: u32 = 0o040_000;
/// POSIX symbolic-link marker. Same value as `S_IFLNK`.
const S_IFLNK: u32 = 0o120_000;

/// `true` when the mode bits encode a regular file.
#[must_use]
pub const fn is_reg(mode: u32) -> bool {
    mode & S_IFMT == S_IFREG
}

/// `true` when the mode bits encode a directory.
#[must_use]
pub const fn is_dir(mode: u32) -> bool {
    mode & S_IFMT == S_IFDIR
}

/// `true` when the mode bits encode a symbolic link.
#[must_use]
pub const fn is_lnk(mode: u32) -> bool {
    mode & S_IFMT == S_IFLNK
}

// =====================================================================
// Flist value-object
// =====================================================================

/// One entry in the rsync file list.
///
/// Direct port of openrsync's `struct flist` (lines 130..135 of
/// `flist.c`) plus the embedded `struct flstat`. The C type splits the
/// stat fields into a nested struct; we collapse the two in Rust
/// because the value object never needs to be partial-borrowed.
///
/// Lock-free contract: the type is `Clone`/`Eq` and lives on the
/// per-session task's stack. Never wrapped in `Arc<Mutex<...>>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flist {
    /// Path relative to the sync root (POSIX `/`-separated). Mirrors
    /// openrsync's `wpath` field — the "working" path the receiver
    /// places into the destination tree.
    pub path: PathBuf,
    /// Symlink target when [`is_lnk`] returns true on [`Self::mode`];
    /// `None` otherwise. Mirrors openrsync's `link` field.
    pub link: Option<PathBuf>,
    /// File size in bytes. Mirrors openrsync's `st.size` (`off_t`).
    pub size: i64,
    /// Modification time in unix seconds. Mirrors openrsync's
    /// `st.mtime` (`time_t`).
    pub mtime: i64,
    /// POSIX mode bits (file-type bits + permission bits). Mirrors
    /// openrsync's `st.mode` (`mode_t`).
    pub mode: u32,
    /// Numeric owner uid. Mirrors openrsync's `st.uid` (`uid_t`).
    pub uid: u32,
    /// Numeric owner gid. Mirrors openrsync's `st.gid` (`gid_t`).
    pub gid: u32,
    /// Per-entry flag bits as observed on the wire. Carries
    /// [`FLIST_TOP_LEVEL`] for top-level directory entries; the
    /// `_SAME` bits are NOT preserved here because they are derived
    /// at send time from the previous entry comparison.
    pub flags: u8,
}

impl Flist {
    /// Build a regular-file entry with empty link target. Used by tests
    /// and the local-walk code path.
    #[must_use]
    pub const fn regular(
        path: PathBuf,
        size: i64,
        mtime: i64,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> Self {
        Self {
            path,
            link: None,
            size,
            mtime,
            mode: (mode & !S_IFMT) | S_IFREG,
            uid,
            gid,
            flags: 0,
        }
    }

    /// Build a directory entry. The mode bits are forced to `S_IFDIR`.
    #[must_use]
    pub const fn directory(path: PathBuf, mtime: i64, mode: u32, uid: u32, gid: u32) -> Self {
        Self {
            path,
            link: None,
            size: 0,
            mtime,
            mode: (mode & !S_IFMT) | S_IFDIR,
            uid,
            gid,
            flags: 0,
        }
    }

    /// Build a symbolic-link entry. The mode bits are forced to
    /// `S_IFLNK`.
    #[must_use]
    pub const fn symlink(
        path: PathBuf,
        target: PathBuf,
        mtime: i64,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> Self {
        Self {
            path,
            link: Some(target),
            size: 0,
            mtime,
            mode: (mode & !S_IFMT) | S_IFLNK,
            uid,
            gid,
            flags: 0,
        }
    }

    /// Wire byte length of the path component. The encoding uses POSIX
    /// `/`-separated bytes, never UTF-16 / OS-specific separators.
    fn path_byte_len(&self) -> usize {
        self.path_bytes().len()
    }

    /// Path component as POSIX bytes. Replaces `\\` with `/` so the
    /// encoder behaves the same on Windows hosts (the rsync wire
    /// format never carries Windows path separators).
    fn path_bytes(&self) -> Vec<u8> {
        path_to_posix_bytes(&self.path)
    }
}

/// Convert a [`Path`] to POSIX `/`-separated bytes. Backslashes are
/// flipped to forward slashes so a Windows host produces the same wire
/// shape as Linux. Lossy UTF-8 conversion is acceptable here — non-UTF
/// pathnames don't survive the rsync wire format anyway.
fn path_to_posix_bytes(path: &Path) -> Vec<u8> {
    let s = path.to_string_lossy();
    s.bytes()
        .map(|b| if b == b'\\' { b'/' } else { b })
        .collect()
}

// =====================================================================
// varint / varlong I/O helpers — port of upstream rsync 3.2.7
// `io.c::read_varint` / `write_varint` / `read_varlong` / `write_varlong`.
// =====================================================================

/// Read a `read_varint30(f)` field — varint at protocol >= 30, plain
/// 4-byte LE int32 below. Mirrors the inline in upstream rsync's
/// `io.h` line 21.
async fn read_varint30<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    negotiated: i32,
) -> Result<i32, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    if negotiated < VARINT_FLIST_MIN_PROTOCOL {
        return reader.read_int(sess).await;
    }
    read_varint(reader, sess).await
}

/// Write a `write_varint30(f, x)` field — varint at protocol >= 30,
/// plain 4-byte LE int32 below. Mirrors the inline in upstream rsync's
/// `io.h` line 37.
async fn write_varint30<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    negotiated: i32,
    val: i32,
) -> Result<(), DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    if negotiated < VARINT_FLIST_MIN_PROTOCOL {
        return writer.write_int(sess, val).await;
    }
    write_varint(writer, sess, val).await
}

/// Port of upstream rsync 3.2.7's `io.c::read_varint` (lines 1794..1825).
/// The first byte's high bits index into [`INT_BYTE_EXTRA`] for the
/// count of follow-up bytes; the remaining payload is little-endian.
async fn read_varint<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
) -> Result<i32, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    let ch = reader.read_byte(sess).await?;
    let extra = usize::from(INT_BYTE_EXTRA[usize::from(ch / 4)]);
    if extra == 0 {
        return Ok(i32::from(ch));
    }
    if extra >= 5 {
        return Err(DomainError::RsyncProtocolError(format!(
            "flist: varint overflow (extra={extra})"
        )));
    }
    let mut tail = [0_u8; 4];
    let slice = tail
        .get_mut(..extra)
        .ok_or_else(|| DomainError::RsyncProtocolError("flist: varint slice OOB".to_string()))?;
    reader.read_buf(sess, slice).await?;
    let bytes = assemble_varint_bytes(ch, &tail[..extra], extra);
    Ok(i32::from_le_bytes(bytes))
}

/// Compose a 4-byte little-endian buffer from the prefix `ch` byte and
/// the `extra`-byte tail per `read_varint`'s post-read assembly. Pure
/// arithmetic — kept separate from [`read_varint`] so the async fn
/// stays under the 30-line cognitive-complexity threshold.
fn assemble_varint_bytes(ch: u8, tail: &[u8], extra: usize) -> [u8; 4] {
    let bit_index = 8_u32.saturating_sub(u32::try_from(extra).unwrap_or(0));
    let bit = 1_u8 << bit_index;
    let mut out = [0_u8; 4];
    let copy_len = extra.min(out.len());
    if let Some(slot) = out.get_mut(..copy_len)
        && let Some(src) = tail.get(..copy_len)
    {
        slot.copy_from_slice(src);
    }
    if extra < out.len()
        && let Some(slot) = out.get_mut(extra)
    {
        *slot = ch & bit.wrapping_sub(1);
    }
    out
}

/// Port of upstream rsync 3.2.7's `io.c::read_varlong` (lines 1826..1865).
async fn read_varlong<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    min_bytes: usize,
) -> Result<i64, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    if min_bytes == 0 || min_bytes > 8 {
        return Err(DomainError::RsyncProtocolError(format!(
            "flist: varlong min_bytes {min_bytes} out of range"
        )));
    }
    let mut head = vec![0_u8; min_bytes];
    reader.read_buf(sess, &mut head).await?;
    let prefix = *head
        .first()
        .ok_or_else(|| DomainError::RsyncProtocolError("flist: varlong empty".to_string()))?;
    let extra = usize::from(INT_BYTE_EXTRA[usize::from(prefix / 4)]);
    if min_bytes.saturating_add(extra) > 9 {
        return Err(DomainError::RsyncProtocolError(format!(
            "flist: varlong overflow (min_bytes={min_bytes}, extra={extra})"
        )));
    }
    let mut tail_buf = vec![0_u8; extra];
    if extra > 0 {
        reader.read_buf(sess, &mut tail_buf).await?;
    }
    let bytes = assemble_varlong_bytes(prefix, &head, &tail_buf, min_bytes, extra);
    Ok(i64::from_le_bytes(bytes))
}

/// Compose an 8-byte little-endian buffer from the `min_bytes`-byte
/// head + `extra`-byte tail per `read_varlong`'s post-read assembly.
/// Pure arithmetic — kept separate from [`read_varlong`] so the async
/// fn stays under the 30-line cognitive-complexity threshold.
fn assemble_varlong_bytes(
    prefix: u8,
    head: &[u8],
    tail: &[u8],
    min_bytes: usize,
    extra: usize,
) -> [u8; 8] {
    let mut buf = [0_u8; 8];
    let head_tail = head.get(1..min_bytes).unwrap_or(&[]);
    if let Some(slot) = buf.get_mut(..head_tail.len()) {
        slot.copy_from_slice(head_tail);
    }
    if extra > 0 {
        let pos = min_bytes.saturating_sub(1);
        if let Some(slot) = buf.get_mut(pos..pos.saturating_add(extra)) {
            let src = tail.get(..extra).unwrap_or(&[]);
            slot.copy_from_slice(src);
        }
        let bit_index = 8_u32.saturating_sub(u32::try_from(extra).unwrap_or(0));
        let bit = 1_u8 << bit_index;
        let head_pos = min_bytes.saturating_add(extra).saturating_sub(1);
        if let Some(slot) = buf.get_mut(head_pos) {
            *slot = prefix & bit.wrapping_sub(1);
        }
    } else if let Some(slot) = buf.get_mut(min_bytes.saturating_sub(1)) {
        *slot = prefix;
    }
    buf
}

/// Port of upstream rsync 3.2.7's `io.c::write_varint` (lines 2088..2107).
async fn write_varint<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    val: i32,
) -> Result<(), DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    let (buf, cnt) = encode_varint(val);
    let out = buf
        .get(..cnt)
        .ok_or_else(|| DomainError::RsyncProtocolError("flist: varint OOB write".to_string()))?;
    writer.write_buf(sess, out).await
}

/// Encode an `i32` into the 5-byte staging buffer used by `write_varint`.
/// Returns the `(buffer, count)` pair the caller writes onto the wire.
/// Pure arithmetic — kept separate so the async fn stays under the
/// 30-line threshold.
fn encode_varint(val: i32) -> ([u8; 5], usize) {
    let mut buf = [0_u8; 5];
    let payload = val.to_le_bytes();
    if let Some(slot) = buf.get_mut(1..5) {
        slot.copy_from_slice(&payload);
    }
    let mut cnt = trim_trailing_zeros(&buf, 4, 1);
    let bit = leading_bit_for_varint(cnt, 1);
    let head_byte = buf.get(cnt).copied().unwrap_or(0);
    if head_byte >= bit {
        cnt = cnt.saturating_add(1);
        if let Some(slot) = buf.first_mut() {
            *slot = !bit.wrapping_sub(1);
        }
    } else if cnt > 1 {
        let bit2 = bit.wrapping_mul(2).wrapping_sub(1);
        let composed = head_byte | !bit2;
        if let Some(slot) = buf.first_mut() {
            *slot = composed;
        }
    } else if let Some(byte_one) = buf.get(1).copied()
        && let Some(slot) = buf.first_mut()
    {
        *slot = byte_one;
    }
    (buf, cnt)
}

/// Walk `buf[start..]` backwards skipping zero bytes; stop at index
/// `min_bytes` (inclusive). Mirrors the trailing-zero strip loop in
/// upstream rsync's `write_varint` / `write_varlong`.
fn trim_trailing_zeros(buf: &[u8], start: usize, min_bytes: usize) -> usize {
    let mut cnt = start;
    while cnt > min_bytes
        && buf
            .get(cnt)
            .copied()
            .is_some_and(|byte_value| byte_value == 0)
    {
        cnt = cnt.saturating_sub(1);
    }
    cnt
}

/// Compute the `bit = 1 << (8 - cnt + min_bytes - 1)` boundary for the
/// varint / varlong head-byte branch. Mirrors the C `bit = ((uchar)1 <<
/// (7 - cnt + min_bytes))` arithmetic.
fn leading_bit_for_varint(cnt: usize, min_bytes: usize) -> u8 {
    let cnt_u32 = u32::try_from(cnt).unwrap_or(0);
    let min_u32 = u32::try_from(min_bytes).unwrap_or(0);
    let shift = 7_u32.saturating_sub(cnt_u32).saturating_add(min_u32);
    1_u8 << shift
}

/// Port of upstream rsync 3.2.7's `io.c::write_varlong` (lines 2110..2150).
async fn write_varlong<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    val: i64,
    min_bytes: usize,
) -> Result<(), DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    if min_bytes == 0 || min_bytes > 8 {
        return Err(DomainError::RsyncProtocolError(format!(
            "flist: varlong min_bytes {min_bytes} out of range"
        )));
    }
    let (buf, cnt) = encode_varlong(val, min_bytes);
    let out = buf
        .get(..cnt)
        .ok_or_else(|| DomainError::RsyncProtocolError("flist: varlong OOB write".to_string()))?;
    writer.write_buf(sess, out).await
}

/// Encode an `i64` into the 9-byte staging buffer used by `write_varlong`.
/// Returns the `(buffer, count)` pair the caller writes onto the wire.
/// Pure arithmetic — kept separate so the async fn stays under the
/// 30-line threshold.
fn encode_varlong(val: i64, min_bytes: usize) -> ([u8; 9], usize) {
    let mut buf = [0_u8; 9];
    let payload = val.to_le_bytes();
    if let Some(slot) = buf.get_mut(1..9) {
        slot.copy_from_slice(&payload);
    }
    let mut cnt = trim_trailing_zeros(&buf, 8, min_bytes);
    let bit = leading_bit_for_varint(cnt, min_bytes);
    let head_byte = buf.get(cnt).copied().unwrap_or(0);
    if head_byte >= bit {
        cnt = cnt.saturating_add(1);
        if let Some(slot) = buf.first_mut() {
            *slot = !bit.wrapping_sub(1);
        }
    } else if cnt > min_bytes {
        let bit2 = bit.wrapping_mul(2).wrapping_sub(1);
        let composed = head_byte | !bit2;
        if let Some(slot) = buf.first_mut() {
            *slot = composed;
        }
    } else if let Some(slot) = buf.first_mut() {
        *slot = head_byte;
    }
    (buf, cnt)
}

// =====================================================================
// recv_flist — port of flist.c::flist_recv (lines 597..795).
// =====================================================================

/// Drain the peer's file list off the wire.
///
/// Direct port of `flist.c::flist_recv`. The driver loops on the flag
/// byte (or 16-bit flag short at protocol 28+) until a zero terminator
/// marks end-of-list. Each entry is decoded against the previous one
/// for the `_SAME` flag collapse.
///
/// `negotiated` carries the post-handshake protocol version so the
/// decoder can branch on the wire-shape boundaries:
///
/// - `negotiated < 28`: 8-bit flag byte, plain `read_int` lengths.
/// - `28..30`: 16-bit `XMIT_EXTENDED_FLAGS` flag short, plain `read_int`
///   lengths.
/// - `>= 30`: 16-bit flag short, `read_varint30` / `read_varlong30`
///   length / size / mtime fields.
///
/// `preserve_uids` / `preserve_gids` mirror `sess->opts->preserve_*`
/// from the C original. `numeric_ids` controls whether the optional
/// trailing user / group lookup tables are read after the terminator.
///
/// Returns the entries in receipt order (openrsync's `flist_recv`
/// additionally `qsort`s by `wpath` and assigns `FLSTAT_TOP_DIR`; we
/// hand that off to the caller because the `qsort` step depends on
/// whether the client is configured for `--recursive` — context this
/// module doesn't carry).
///
/// # Errors
///
/// - [`DomainError::RsyncProtocolError`] on transport failure, EOF
///   mid-read, malformed flag byte, zero-length pathname (rsync's
///   "security violation" check), or absolute / backtracking path.
pub async fn recv_flist<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    opts: FlistRecvOpts,
    negotiated: i32,
) -> Result<Vec<Flist>, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    let mut entries: Vec<Flist> = Vec::new();
    let mut last_path: Vec<u8> = Vec::new();
    loop {
        let flags = read_entry_flags(reader, sess, negotiated).await?;
        if flags == 0 {
            break;
        }
        if is_io_error_endlist_marker(flags) {
            drain_io_error_endlist(reader, sess).await?;
            break;
        }
        let entry = recv_one_entry(
            reader,
            sess,
            &opts,
            flags,
            &last_path,
            entries.last(),
            negotiated,
        )
        .await?;
        log_recv_entry(&entry, entries.len(), flags);
        last_path = path_to_posix_bytes(&entry.path);
        entries.push(entry);
    }
    Ok(entries)
}

/// Drain the proto-31+ end-of-list `io_error` counter and surface a
/// non-zero count via `tracing::warn!`. Pulled out of [`recv_flist`]
/// so the public fn stays under the 30-line cognitive-complexity
/// threshold.
///
/// Mirrors rsync 3.2.7's `flist.c::recv_file_list` lines 2631..2640.
async fn drain_io_error_endlist<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
) -> Result<(), DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    let err = read_varint(reader, sess).await?;
    if err != 0 {
        tracing::warn!(
            target: "rsync.wire.flist",
            io_error_count = err,
            "sender reported io_error_count on end-of-list frame"
        );
    }
    Ok(())
}

/// Emit the per-entry diagnostic trace for a successful decode. Pulled
/// out of [`recv_flist`] so the public fn stays under the 30-line
/// cognitive-complexity threshold.
fn log_recv_entry(entry: &Flist, idx: usize, wire_flags: u16) {
    tracing::debug!(
        target: "rsync.wire.flist",
        idx = idx,
        path = %entry.path.display(),
        mode = format!("{:#o}", entry.mode),
        size = entry.size,
        mtime = entry.mtime,
        wire_flags = format!("{wire_flags:#06x}"),
        "recv flist entry"
    );
}

/// `true` when the 16-bit flag short carries the proto-31+ end-of-list
/// `io_error` sentinel (`XMIT_EXTENDED_FLAGS | XMIT_IO_ERROR_ENDLIST`).
/// Mirrors rsync 3.2.7's `flist.c::recv_file_list` line 2631.
const fn is_io_error_endlist_marker(flags: u16) -> bool {
    flags == (XMIT_EXTENDED_FLAGS | XMIT_IO_ERROR_ENDLIST)
}

/// Read the flag short for one flist entry.
///
/// At protocol < 28 the wire shape is a single byte. At protocol >= 28,
/// the low byte is read first; if `XMIT_EXTENDED_FLAGS` is set, a high
/// byte follows and is OR-shifted `<< 8`. Mirrors the inner-loop
/// branch of upstream rsync 3.2.7's `flist.c::recv_file_list` (lines
/// 2604..2618).
async fn read_entry_flags<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    negotiated: i32,
) -> Result<u16, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    let low = reader.read_byte(sess).await?;
    if low == 0 {
        return Ok(0);
    }
    if negotiated < XMIT_EXTENDED_FLAGS_MIN_PROTOCOL {
        return Ok(u16::from(low));
    }
    let mut flags = u16::from(low);
    if flags & XMIT_EXTENDED_FLAGS != 0 {
        let high = reader.read_byte(sess).await?;
        flags |= u16::from(high) << 8_u32;
    }
    Ok(flags)
}

/// Per-recv options. Mirrors the subset of `sess->opts` the
/// `flist_recv` path consults.
#[derive(Debug, Clone, Copy, Default)]
pub struct FlistRecvOpts {
    /// Set when the peer is sending uid bytes per entry (rsync `-o`).
    pub preserve_uids: bool,
    /// Set when the peer is sending gid bytes per entry (rsync `-g`).
    pub preserve_gids: bool,
    /// Set when the peer is sending symlink-target bytes for `S_ISLNK`
    /// entries (rsync `-l`).
    pub preserve_links: bool,
}

/// Decode one [`Flist`] entry once the flag byte / short has been
/// pulled.
///
/// Mirrors the for-loop body of openrsync's `flist_recv` plus the
/// proto-30+ varint30 / varlong30 length fields per upstream rsync
/// 3.2.7's `flist.c::recv_file_entry`. Method-local only (caller
/// verifies flags != 0 first).
async fn recv_one_entry<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    opts: &FlistRecvOpts,
    flags: u16,
    last_path: &[u8],
    last_entry: Option<&Flist>,
    negotiated: i32,
) -> Result<Flist, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    let flag_low = u8::try_from(flags & 0x00ff).unwrap_or(0);
    let path = recv_name(reader, sess, flag_low, last_path, negotiated).await?;
    // Proto 30+ hardlink-follower entries forward-reference an earlier
    // entry's index via `read_varint` between the name and the size.
    // We pull the index off the wire and discard it — the receiver's
    // hardlink-apply pass will reuse the leader's payload via the
    // already-decoded path list. (rsync 3.2.7 `flist.c::recv_file_entry`
    // lines 778..824. Note: `BITS_SETnUNSET(xflags, HLINKED, HLINK_FIRST)`
    // = follower; leader has both bits set and skips the read.)
    skip_hardlink_ref(reader, sess, flags, negotiated).await?;
    let size = recv_file_size(reader, sess, negotiated).await?;
    let mtime = recv_mtime(reader, sess, flag_low, negotiated, last_entry).await?;
    skip_mod_nsec(reader, sess, flags).await?;
    let mode = if (flag_low & FLIST_MODE_SAME) == 0 {
        cast_i32_to_u32(reader.read_int(sess).await?)
    } else {
        last_entry.map_or(0, |e| e.mode)
    };
    let uid = recv_uid(reader, sess, opts, flags, last_entry, negotiated).await?;
    let gid = recv_gid(reader, sess, opts, flags, last_entry, negotiated).await?;
    let link = recv_link(reader, sess, opts, mode, negotiated).await?;
    Ok(Flist {
        path,
        link,
        size,
        mtime,
        mode,
        uid,
        gid,
        flags: flag_low & FLIST_TOP_LEVEL,
    })
}

/// Pull and discard the hardlink reference index when the entry is a
/// proto-30+ hardlink follower. Mirrors rsync 3.2.7's
/// `flist.c::recv_file_entry` lines 779..824 — the abbreviated-follower
/// branch of `BITS_SETnUNSET(xflags, XMIT_HLINKED, XMIT_HLINK_FIRST)`.
async fn skip_hardlink_ref<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    flags: u16,
    negotiated: i32,
) -> Result<(), DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    if negotiated < VARINT_FLIST_MIN_PROTOCOL {
        return Ok(());
    }
    let is_hardlink_follower = (flags & XMIT_HLINKED) != 0 && (flags & XMIT_HLINK_FIRST) == 0;
    if !is_hardlink_follower {
        return Ok(());
    }
    let _ndx = read_varint(reader, sess).await?;
    Ok(())
}

/// Pull and discard the modification-time nanoseconds suffix (proto
/// 31+, when [`XMIT_MOD_NSEC`] is set). The `Flist` value-object only
/// preserves whole-second mtimes; future slices wanting nanosecond
/// precision can lift this read into a struct field. Mirrors
/// rsync 3.2.7's `flist.c::recv_file_entry` lines 841..847.
async fn skip_mod_nsec<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    flags: u16,
) -> Result<(), DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    if (flags & XMIT_MOD_NSEC) == 0 {
        return Ok(());
    }
    let _nsec = read_varint(reader, sess).await?;
    Ok(())
}

/// Decode the per-entry file size. At protocol < 30 this is the
/// `recv_long` varint64 (i32 head + i64 sentinel). At protocol >= 30
/// this is `read_varlong(_, 3)`. Mirrors `write_varlong30(_, 3)` on
/// the sender side.
async fn recv_file_size<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    negotiated: i32,
) -> Result<i64, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    if negotiated < VARINT_FLIST_MIN_PROTOCOL {
        return recv_long(reader, sess).await;
    }
    read_varlong(reader, sess, 3).await
}

/// Decode the per-entry mtime (when `FLIST_TIME_SAME` is unset). At
/// protocol < 30 this is `read_int` (4 bytes LE). At protocol >= 30
/// this is `read_varlong(_, 4)`. Mirrors upstream rsync 3.2.7's
/// `flist.c::recv_file_entry` modtime branch.
async fn recv_mtime<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    flag_low: u8,
    negotiated: i32,
    last_entry: Option<&Flist>,
) -> Result<i64, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    if (flag_low & FLIST_TIME_SAME) != 0 {
        return Ok(last_entry.map_or(0, |e| e.mtime));
    }
    if negotiated < VARINT_FLIST_MIN_PROTOCOL {
        return Ok(i64::from(reader.read_int(sess).await?));
    }
    read_varlong(reader, sess, 4).await
}

/// Decode the optional uid field. Mirrors the `if preserve_uids` block
/// of openrsync's `flist_recv` plus the proto-30+ varint shape and
/// the [`XMIT_USER_NAME_FOLLOWS`] username-string suffix.
async fn recv_uid<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    opts: &FlistRecvOpts,
    flags: u16,
    last_entry: Option<&Flist>,
    negotiated: i32,
) -> Result<u32, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    if !opts.preserve_uids {
        return Ok(0);
    }
    let flag_low = u8::try_from(flags & 0x00ff).unwrap_or(0);
    if (flag_low & FLIST_UID_SAME) != 0 {
        return Ok(last_entry.map_or(0, |e| e.uid));
    }
    let raw = read_varint30(reader, sess, negotiated).await?;
    if (flags & XMIT_USER_NAME_FOLLOWS) != 0 {
        skip_id_name_suffix(reader, sess).await?;
    }
    Ok(cast_i32_to_u32(raw))
}

/// Decode the optional gid field. Mirrors the `if preserve_gids` block
/// of openrsync's `flist_recv` plus the proto-30+ varint shape and
/// the [`XMIT_GROUP_NAME_FOLLOWS`] groupname-string suffix.
async fn recv_gid<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    opts: &FlistRecvOpts,
    flags: u16,
    last_entry: Option<&Flist>,
    negotiated: i32,
) -> Result<u32, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    if !opts.preserve_gids {
        return Ok(0);
    }
    let flag_low = u8::try_from(flags & 0x00ff).unwrap_or(0);
    if (flag_low & FLIST_GID_SAME) != 0 {
        return Ok(last_entry.map_or(0, |e| e.gid));
    }
    let raw = read_varint30(reader, sess, negotiated).await?;
    if (flags & XMIT_GROUP_NAME_FOLLOWS) != 0 {
        skip_id_name_suffix(reader, sess).await?;
    }
    Ok(cast_i32_to_u32(raw))
}

/// Pull and discard a `read_byte`-prefixed username / groupname string.
/// Mirrors rsync 3.2.7's `uidlist.c::recv_user_name` and
/// `recv_group_name` length+payload codec — the value is discarded
/// because the [`Flist`] value-object stores numeric ids only and the
/// receiver re-resolves names locally during the apply pass.
async fn skip_id_name_suffix<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
) -> Result<(), DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    let len = usize::from(reader.read_byte(sess).await?);
    if len == 0 {
        return Ok(());
    }
    let mut buf = vec![0_u8; len];
    reader.read_buf(sess, &mut buf).await?;
    Ok(())
}

/// Decode the optional symlink-target field. Mirrors the `if S_ISLNK +
/// preserve_links` block of openrsync's `flist_recv`. Symlink length
/// is `read_varint30` so it is varint at protocol >= 30, plain int32
/// below.
async fn recv_link<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    opts: &FlistRecvOpts,
    mode: u32,
    negotiated: i32,
) -> Result<Option<PathBuf>, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    if !(is_lnk(mode) && opts.preserve_links) {
        return Ok(None);
    }
    let len_raw = read_varint30(reader, sess, negotiated).await?;
    let len = usize::try_from(len_raw).map_err(|err| {
        DomainError::RsyncProtocolError(format!(
            "flist: symlink length {len_raw} not a usize: {err}"
        ))
    })?;
    if len == 0 {
        return Err(DomainError::RsyncProtocolError(
            "flist: empty symlink target".to_string(),
        ));
    }
    let mut buf = vec![0_u8; len];
    reader.read_buf(sess, &mut buf).await?;
    let s = String::from_utf8(buf).map_err(|e| {
        DomainError::RsyncProtocolError(format!("flist: non-utf8 symlink target: {e}"))
    })?;
    Ok(Some(PathBuf::from(s)))
}

/// Decode an entry's pathname. Mirrors `flist.c::flist_recv_name`
/// (lines 438..519) plus the proto-30+ `read_varint30` long-name shape.
async fn recv_name<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    flag_low: u8,
    last_path: &[u8],
    negotiated: i32,
) -> Result<PathBuf, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    let (prefix, remainder) = recv_name_lengths(reader, sess, flag_low, negotiated).await?;
    let total = prefix.saturating_add(remainder);
    if total == 0 {
        return Err(DomainError::RsyncProtocolError(
            "flist: zero-length pathname (security violation)".to_string(),
        ));
    }
    let buf = recv_name_bytes(reader, sess, last_path, prefix, remainder).await?;
    let s = String::from_utf8(buf)
        .map_err(|e| DomainError::RsyncProtocolError(format!("flist: non-utf8 pathname: {e}")))?;
    validate_path_safety(&s)?;
    Ok(PathBuf::from(s))
}

/// Pull the `(prefix, remainder)` pair off the wire per the
/// `FLIST_NAME_SAME` / `FLIST_NAME_LONG` flag matrix. The remainder
/// length uses `read_varint30` at protocol >= 30 (i.e. varint), plain
/// `read_int` below.
async fn recv_name_lengths<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    flag_low: u8,
    negotiated: i32,
) -> Result<(usize, usize), DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    let prefix = if (flag_low & FLIST_NAME_SAME) == 0 {
        0_usize
    } else {
        usize::from(reader.read_byte(sess).await?)
    };
    let remainder = if (flag_low & FLIST_NAME_LONG) == 0 {
        usize::from(reader.read_byte(sess).await?)
    } else {
        let len = read_varint30(reader, sess, negotiated).await?;
        usize::try_from(len).map_err(|err| {
            DomainError::RsyncProtocolError(format!(
                "flist: long-name length {len} not a usize: {err}"
            ))
        })?
    };
    Ok((prefix, remainder))
}

/// Concatenate the inherited prefix and the freshly-read remainder
/// into the entry's full POSIX-byte path.
async fn recv_name_bytes<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
    last_path: &[u8],
    prefix: usize,
    remainder: usize,
) -> Result<Vec<u8>, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    let mut buf = Vec::with_capacity(prefix.saturating_add(remainder));
    if prefix > 0 {
        // Upstream rsync 3.2.7 `flist.c::recv_file_entry` line 731 uses
        // `strlcpy(thisname, lastname, l1+1)`, which silently caps at
        // `strlen(lastname)`. If the wire prefix exceeds last_path's
        // length, rsync simply copies what's available and lets the
        // server-side encoder's prefix-vs-length asymmetry resolve via
        // the trailing remainder bytes. We replicate that semantics
        // rather than treating the over-long prefix as a protocol
        // violation.
        let take = prefix.min(last_path.len());
        if let Some(slice) = last_path.get(..take) {
            buf.extend_from_slice(slice);
        }
    }
    let mut tail = vec![0_u8; remainder];
    if remainder > 0 {
        reader.read_buf(sess, &mut tail).await?;
    }
    buf.extend_from_slice(&tail);
    Ok(buf)
}

/// Apply the rsync "security violation" checks — reject absolute
/// paths, `..` segments, and paths leading into the parent.
fn validate_path_safety(s: &str) -> Result<(), DomainError> {
    if let Some(first) = s.as_bytes().first()
        && *first == b'/'
    {
        return Err(DomainError::RsyncProtocolError(format!(
            "flist: absolute pathname rejected (security violation): {s}"
        )));
    }
    if path_has_backtrack(s) {
        return Err(DomainError::RsyncProtocolError(format!(
            "flist: backtracking pathname rejected (security violation): {s}"
        )));
    }
    Ok(())
}

/// Mirror of openrsync's `flist_recv_name` backtrack check (lines
/// 504..512).
fn path_has_backtrack(s: &str) -> bool {
    s == ".." || s.starts_with("../") || s.ends_with("/..") || s.contains("/../")
}

/// Decode a varint64 (rsync `int64`). Mirrors `io.c::io_read_long`
/// (lines 558..604): a leading i32 of `-1` flags a 64-bit follow-up,
/// otherwise the i32 is the value.
async fn recv_long<R>(
    reader: &mut MplexReader<R>,
    sess: &mut WireSession,
) -> Result<i64, DomainError>
where
    R: AsyncRead + Unpin + Send,
{
    let head = reader.read_int(sess).await?;
    if head != -1 {
        return Ok(i64::from(head));
    }
    let mut buf = [0_u8; 8];
    reader.read_buf(sess, &mut buf).await?;
    Ok(i64::from_le_bytes(buf))
}

/// Reinterpret an `i32` wire value as a `u32` field (mode / uid / gid).
/// Mirrors openrsync's implicit `(uint32_t)` cast.
const fn cast_i32_to_u32(v: i32) -> u32 {
    u32::from_ne_bytes(v.to_ne_bytes())
}

// =====================================================================
// send_flist — port of flist.c::flist_send (lines 264..428).
// =====================================================================

/// Per-send options. Mirrors the subset of `sess->opts` the
/// `flist_send` path consults.
#[derive(Debug, Clone, Copy, Default)]
pub struct FlistSendOpts {
    /// Set when the peer wants per-entry uid bytes (rsync `-o`).
    pub preserve_uids: bool,
    /// Set when the peer wants per-entry gid bytes (rsync `-g`).
    pub preserve_gids: bool,
    /// Set when the peer wants symlink-target bytes for `S_ISLNK`
    /// entries (rsync `-l`).
    pub preserve_links: bool,
}

/// Serialise a file list onto the wire.
///
/// Direct port of openrsync's `flist_send` plus the proto-28+ 16-bit
/// `XMIT_EXTENDED_FLAGS` flag short and proto-30+ varint30 / varlong30
/// length / size / mtime fields per upstream rsync 3.2.7's
/// `flist.c::send_file_entry`.
///
/// Wire-shape branching:
///
/// - `negotiated < 28`: 8-bit flag byte, plain `write_int` lengths.
/// - `28..30`: 16-bit flag short via `XMIT_EXTENDED_FLAGS`, plain
///   `write_int` lengths.
/// - `>= 30`: 16-bit flag short, `write_varint30` / `write_varlong30`
///   length / size / mtime fields.
///
/// Each entry is encoded with `XMIT_LONG_NAME` (== [`FLIST_NAME_LONG`])
/// pinned so the name length always travels as a (var)int rather than
/// a single byte — this matches openrsync's "for ease, make all of our
/// filenames be 'long'" comment.
///
/// `total_size` is bumped per regular-file entry to mirror openrsync's
/// `sess->total_size += f->st.size` accounting.
///
/// # Errors
///
/// Returns [`DomainError::RsyncProtocolError`] on transport failure or
/// when an entry's path is empty (the rsync wire format reserves the
/// zero byte for the end-of-list terminator).
pub async fn send_flist<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    entries: &[Flist],
    opts: FlistSendOpts,
    negotiated: i32,
) -> Result<(), DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    for entry in entries {
        send_one_entry(writer, sess, entry, &opts, negotiated).await?;
        if is_reg(entry.mode) {
            sess.total_size = sess
                .total_size
                .saturating_add(u64::try_from(entry.size).unwrap_or(0));
        }
    }
    // End-of-list sentinel — `write_byte(0)` at every protocol per
    // upstream rsync 3.2.7's `flist.c::write_end_of_flist` line 2080
    // (no io_error to report).
    writer.write_byte(sess, 0).await?;
    Ok(())
}

/// Emit a single entry. The encoder always pins `XMIT_LONG_NAME` so
/// the name length travels as a (var)int. At protocol 28+ the encoder
/// also pins `XMIT_EXTENDED_FLAGS` so the flag field always travels as
/// a 16-bit `write_shortint` short.
async fn send_one_entry<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    entry: &Flist,
    opts: &FlistSendOpts,
    negotiated: i32,
) -> Result<(), DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    send_name_header(writer, sess, entry, negotiated).await?;
    send_file_size(writer, sess, entry.size, negotiated).await?;
    send_mtime(writer, sess, entry.mtime, negotiated).await?;
    let mode_i32 = i32::from_ne_bytes(entry.mode.to_ne_bytes());
    writer.write_int(sess, mode_i32).await?;
    send_owner_fields(writer, sess, entry, opts, negotiated).await?;
    if is_lnk(entry.mode) && opts.preserve_links {
        send_symlink_target(writer, sess, entry, negotiated).await?;
    }
    Ok(())
}

/// Emit the flag short + name length + path bytes for one entry.
///
/// At protocol 28+ the flag travels as a 16-bit `write_shortint` low
/// byte first, high byte second. The encoder always pins
/// `XMIT_EXTENDED_FLAGS` (so the high byte is always emitted) and
/// `XMIT_LONG_NAME` (so the name length is always a varint at proto
/// >= 30, plain int below).
async fn send_name_header<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    entry: &Flist,
    negotiated: i32,
) -> Result<(), DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    let path_bytes = entry.path_bytes();
    if path_bytes.is_empty() {
        return Err(DomainError::RsyncProtocolError(
            "flist: cannot send entry with empty path".to_string(),
        ));
    }
    let len_i32 = i32::try_from(path_bytes.len()).map_err(|err| {
        DomainError::RsyncProtocolError(format!(
            "flist: path length {} > i32::MAX: {err}",
            entry.path_byte_len()
        ))
    })?;
    let mut flags16: u16 = XMIT_LONG_NAME_16;
    if (entry.flags & FLIST_TOP_LEVEL) != 0 {
        flags16 |= XMIT_TOP_DIR_16;
    }
    write_entry_flags(writer, sess, flags16, negotiated).await?;
    write_varint30(writer, sess, negotiated, len_i32).await?;
    writer.write_buf(sess, &path_bytes).await?;
    Ok(())
}

/// Emit the flag field for one flist entry.
///
/// At protocol < 28 the wire shape is a single byte. At protocol >= 28,
/// the encoder always sets `XMIT_EXTENDED_FLAGS` and emits two bytes
/// (low byte first, high byte second) so the receiver always pulls a
/// 16-bit short. Mirrors the `write_shortint` short-int branch of
/// upstream rsync 3.2.7's `flist.c::send_file_entry` (lines 549..553),
/// simplified to always emit the short.
async fn write_entry_flags<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    flags: u16,
    negotiated: i32,
) -> Result<(), DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    if negotiated < XMIT_EXTENDED_FLAGS_MIN_PROTOCOL {
        let one_byte = u8::try_from(flags & 0x00ff).unwrap_or(0);
        return writer.write_byte(sess, one_byte).await;
    }
    let extended = flags | XMIT_EXTENDED_FLAGS;
    let low = u8::try_from(extended & 0x00ff).unwrap_or(0);
    let high = u8::try_from((extended >> 8_u32) & 0x00ff).unwrap_or(0);
    writer.write_buf(sess, &[low, high]).await
}

/// Emit the per-entry file size. At protocol < 30 this is the legacy
/// `send_long` (i32 + i64-sentinel). At protocol >= 30 it is
/// `write_varlong(_, 3)` per upstream rsync 3.2.7's
/// `flist.c::send_file_entry` line 580.
async fn send_file_size<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    size: i64,
    negotiated: i32,
) -> Result<(), DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    if negotiated < VARINT_FLIST_MIN_PROTOCOL {
        return send_long(writer, sess, size).await;
    }
    write_varlong(writer, sess, size, 3).await
}

/// Emit the per-entry mtime. At protocol < 30 this is `write_int`
/// (4 bytes LE). At protocol >= 30 it is `write_varlong(_, 4)` per
/// upstream rsync 3.2.7's `flist.c::send_file_entry` lines 581..585.
/// We never set `XMIT_SAME_TIME` because the encoder writes a fresh
/// mtime per entry — same as openrsync.
async fn send_mtime<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    mtime: i64,
    negotiated: i32,
) -> Result<(), DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    if negotiated < VARINT_FLIST_MIN_PROTOCOL {
        let mtime_i32 = i32::try_from(mtime & 0xffff_ffff_i64).unwrap_or(0);
        return writer.write_int(sess, mtime_i32).await;
    }
    write_varlong(writer, sess, mtime, 4).await
}

/// Emit the optional uid / gid fields per [`FlistSendOpts`]. At
/// protocol >= 30 these are varint-encoded (`write_varint30`) per
/// upstream rsync 3.2.7's `flist.c::send_file_entry` lines 597..617.
async fn send_owner_fields<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    entry: &Flist,
    opts: &FlistSendOpts,
    negotiated: i32,
) -> Result<(), DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    if opts.preserve_uids {
        let uid_i32 = i32::from_ne_bytes(entry.uid.to_ne_bytes());
        write_varint30(writer, sess, negotiated, uid_i32).await?;
    }
    if opts.preserve_gids {
        let gid_i32 = i32::from_ne_bytes(entry.gid.to_ne_bytes());
        write_varint30(writer, sess, negotiated, gid_i32).await?;
    }
    Ok(())
}

/// Emit a symlink-target field. Empty targets are forbidden by the
/// rsync wire shape — we mirror openrsync's `assert(sz < INT32_MAX)`
/// implicit-non-zero contract. The length is `write_varint30` so it
/// is varint at protocol >= 30, plain int32 below.
async fn send_symlink_target<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    entry: &Flist,
    negotiated: i32,
) -> Result<(), DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    let target = entry.link.as_ref().ok_or_else(|| {
        DomainError::RsyncProtocolError(format!(
            "flist: symlink entry {} missing link target",
            entry.path.display()
        ))
    })?;
    let target_bytes = path_to_posix_bytes(target);
    if target_bytes.is_empty() {
        return Err(DomainError::RsyncProtocolError(format!(
            "flist: symlink entry {} has empty link target",
            entry.path.display()
        )));
    }
    let len_i32 = i32::try_from(target_bytes.len()).map_err(|err| {
        DomainError::RsyncProtocolError(format!(
            "flist: symlink target length {} > i32::MAX: {err}",
            target_bytes.len()
        ))
    })?;
    write_varint30(writer, sess, negotiated, len_i32).await?;
    writer.write_buf(sess, &target_bytes).await?;
    Ok(())
}

/// Encode a varint64. Mirrors `io.c::io_write_ulong` (lines 390..418):
/// in-range positive values fit a single i32, otherwise emit `-1`
/// followed by the full 8-byte LE u64.
async fn send_long<W>(
    writer: &mut MplexWriter<W>,
    sess: &mut WireSession,
    val: i64,
) -> Result<(), DomainError>
where
    W: AsyncWrite + Unpin + Send,
{
    if (0..=i64::from(i32::MAX)).contains(&val) {
        let truncated = i32::try_from(val).unwrap_or(0);
        return writer.write_int(sess, truncated).await;
    }
    writer.write_int(sess, -1).await?;
    let unsigned = u64::from_ne_bytes(val.to_ne_bytes());
    writer.write_buf(sess, &unsigned.to_le_bytes()).await
}

// =====================================================================
// gen_flist_local — minimal local-walk producing a sorted Vec<Flist>.
// =====================================================================

/// Walk a local directory tree and produce a sorted file list.
///
/// Equivalent to openrsync's `flist_gen_dirent` (with FTS) but
/// rewritten async + lock-free. Path filters / `--exclude` / `--include`
/// matching ship in slice 3 alongside the sender state machine; this
/// slice's caller is the wire transport's session driver which always
/// passes `tx_filters = empty filter list`.
///
/// Returns entries sorted lexicographically by their wire-relative
/// path. The walk root itself appears as `.` so the rsync 3.x server
/// recognises it as the top-level directory.
///
/// # Errors
///
/// - [`DomainError::RsyncProtocolError`] on filesystem-walk failure
///   (the caller wraps it as a session start error).
pub async fn gen_flist_local(root: &Path) -> Result<Vec<Flist>, DomainError> {
    gen_flist_local_with_opts(root, PreserveFlags::none()).await
}

/// Slice 9 — walker variant that consults [`PreserveFlags`] before
/// stamping each entry with the synthetic default permission bits.
///
/// When `preserve.perms` is set, regular-file / directory entries carry
/// the real on-disk mode bits (UNIX side only — Windows hosts always
/// use the synthetic defaults because `MetadataExt::mode` is not
/// available there). When `preserve.mtime` is unset the walker zeroes
/// the per-entry mtime so the wire never carries a fingerprint that the
/// remote could pin against. When `preserve.links` is unset symlink
/// entries collapse to "synthetic regular file with the link's
/// resolved target size" — the receiver-side server will refuse to
/// follow the link without the `-l` flag, mirroring upstream rsync's
/// non-`-l` semantics. When `preserve.owner` / `preserve.group` are set
/// the walker emits the real uid / gid.
///
/// # Errors
///
/// Same shape as [`gen_flist_local`].
pub async fn gen_flist_local_with_opts(
    root: &Path,
    preserve: PreserveFlags,
) -> Result<Vec<Flist>, DomainError> {
    gen_flist_local_with_filters(root, preserve, None).await
}

/// Bug-B fix — slice 9.5 — walker variant that consults
/// gitignore-style include / exclude globsets before adding entries to
/// the flist.
///
/// Match semantics mirror the SFTP walker (see
/// [`crate::adapters::rsync::sftp::walker`]):
///
/// - When `filters` is `None`, every entry passes through (back-compat
///   with the legacy [`gen_flist_local`] / [`gen_flist_local_with_opts`]
///   callers).
/// - When `filters` is `Some(...)`, the entry's `/`-separated relative
///   path (POSIX style — Windows hosts also use forward slashes on the
///   wire) is matched against `excludes`. Entries that match are
///   dropped, unless the non-empty `includes` set rescues them.
/// - Excluded directories are NOT recursed into, mirroring the SFTP
///   walker — unless `includes` is non-empty (rsync's "exclude this dir
///   but include these files inside it" semantics requires recursion to
///   re-test the children).
///
/// # Errors
///
/// Same shape as [`gen_flist_local`]. Globset compilation lives at the
/// caller site (the wire transport's [`crate::adapters::rsync::wire`]
/// module builds the sets from the inbound `RsyncStartRequest`).
pub async fn gen_flist_local_with_filters(
    root: &Path,
    preserve: PreserveFlags,
    filters: Option<&FlistFilters<'_>>,
) -> Result<Vec<Flist>, DomainError> {
    let metadata = fs::metadata(root).await.map_err(|e| {
        DomainError::RsyncProtocolError(format!("gen_flist: lstat({}): {e}", root.display()))
    })?;
    if !metadata.is_dir() {
        return Err(DomainError::RsyncProtocolError(format!(
            "gen_flist: root {} is not a directory",
            root.display()
        )));
    }
    let mut out: Vec<Flist> = Vec::new();
    out.push(Flist {
        path: PathBuf::from("."),
        link: None,
        size: 0,
        mtime: pick_mtime(0, preserve),
        mode: pick_mode(&metadata, true, preserve),
        uid: pick_uid(&metadata, preserve),
        gid: pick_gid(&metadata, preserve),
        flags: FLIST_TOP_LEVEL,
    });
    walk_into(root, preserve, filters, &mut out).await?;
    sort_entries(&mut out);
    Ok(out)
}

/// Compiled exclude / include globsets handed to the wire-side walker
/// for Bug-B filtering. Lifetime tied to the caller (the wire
/// transport owns the globsets for the life of the session).
#[derive(Debug, Clone, Copy)]
pub struct FlistFilters<'a> {
    /// Matched entries are dropped (unless rescued by `includes`).
    pub excludes: &'a GlobSet,
    /// When non-empty, an include match overrides a matching exclude.
    pub includes: &'a GlobSet,
}

impl FlistFilters<'_> {
    /// Decide whether the entry at `rel` (POSIX `/`-separated path)
    /// passes the filter pipeline.
    fn is_included(&self, rel: &str) -> bool {
        let excluded = !self.excludes.is_empty() && self.excludes.is_match(rel);
        if !excluded {
            return true;
        }
        if self.includes.is_empty() {
            return false;
        }
        self.includes.is_match(rel)
    }
}

/// Iterate a directory tree using an explicit stack (avoids the
/// `Box<dyn Future>` recursion-via-async-fn footgun).
async fn walk_into(
    root: &Path,
    preserve: PreserveFlags,
    filters: Option<&FlistFilters<'_>>,
    out: &mut Vec<Flist>,
) -> Result<(), DomainError> {
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        push_dir_entries(root, &dir, preserve, filters, &mut stack, out).await?;
    }
    Ok(())
}

/// Drain one directory's `read_dir` results into `out`, queueing
/// nested directories onto `stack` for the next outer iteration.
async fn push_dir_entries(
    root: &Path,
    dir: &Path,
    preserve: PreserveFlags,
    filters: Option<&FlistFilters<'_>>,
    stack: &mut Vec<PathBuf>,
    out: &mut Vec<Flist>,
) -> Result<(), DomainError> {
    let mut readdir = fs::read_dir(dir).await.map_err(|e| {
        DomainError::RsyncProtocolError(format!("gen_flist: readdir({}): {e}", dir.display()))
    })?;
    while let Some(entry) = readdir
        .next_entry()
        .await
        .map_err(|e| DomainError::RsyncProtocolError(format!("gen_flist: readdir-next: {e}")))?
    {
        process_dir_entry(root, &entry, preserve, filters, stack, out).await?;
    }
    Ok(())
}

/// Process a single `read_dir` result: lstat the path, build the
/// [`Flist`] entry, decide whether the per-call filter set keeps it,
/// and decide whether to recurse into the directory.
async fn process_dir_entry(
    root: &Path,
    entry: &DirEntry,
    preserve: PreserveFlags,
    filters: Option<&FlistFilters<'_>>,
    stack: &mut Vec<PathBuf>,
    out: &mut Vec<Flist>,
) -> Result<(), DomainError> {
    let entry_path = entry.path();
    let metadata = fs::symlink_metadata(&entry_path).await.map_err(|e| {
        DomainError::RsyncProtocolError(format!("gen_flist: lstat({}): {e}", entry_path.display()))
    })?;
    // Mirror upstream rsync 3.2.7's `flist.c::send_file_name` skip-on-
    // non-regular guard (lines 2090-2098): when `-l` (preserve.links)
    // is not negotiated, drop symlinks at the walker before they reach
    // the wire flist. Without this filter the server emits "skipping
    // non-regular file" Info advisories and the per-file ack stream
    // diverges from our sender state machine — the session deadlines
    // out without progress.
    if metadata.file_type().is_symlink() && !preserve.links {
        return Ok(());
    }
    let rel = entry_path
        .strip_prefix(root)
        .map_or_else(|_| entry_path.clone(), Path::to_path_buf);
    let rel_posix = path_as_posix(&rel);
    let entry_pass = filters.is_none_or(|f| f.is_included(&rel_posix));
    let item = build_flist_entry(&entry_path, &metadata, rel, preserve).await?;
    // Skip pruned directories entirely when there's no rescue
    // include set; otherwise still recurse so children can be
    // re-tested by the include matcher.
    let recurse =
        is_dir(item.mode) && (entry_pass || filters.is_some_and(|f| !f.includes.is_empty()));
    if recurse {
        stack.push(entry_path);
    }
    if entry_pass {
        out.push(item);
    }
    Ok(())
}

/// Render a [`Path`] as a `/`-separated string for globset matching.
/// Matches the SFTP walker's POSIX-only convention so cross-platform
/// callers see the same match semantics regardless of host OS.
fn path_as_posix(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Build a single [`Flist`] entry from a [`Metadata`] result.
async fn build_flist_entry(
    fs_path: &Path,
    metadata: &Metadata,
    rel: PathBuf,
    preserve: PreserveFlags,
) -> Result<Flist, DomainError> {
    let mtime = pick_mtime(metadata_mtime(metadata), preserve);
    if metadata.is_dir() {
        return Ok(Flist {
            path: rel,
            link: None,
            size: 0,
            mtime,
            mode: pick_mode(metadata, true, preserve),
            uid: pick_uid(metadata, preserve),
            gid: pick_gid(metadata, preserve),
            flags: 0,
        });
    }
    if metadata.file_type().is_symlink() {
        return build_symlink_entry(fs_path, rel, mtime, metadata, preserve).await;
    }
    let size = i64::try_from(metadata.len()).unwrap_or(i64::MAX);
    Ok(Flist {
        path: rel,
        link: None,
        size,
        mtime,
        mode: pick_mode(metadata, false, preserve),
        uid: pick_uid(metadata, preserve),
        gid: pick_gid(metadata, preserve),
        flags: 0,
    })
}

/// Resolve the symlink target and emit the matching [`Flist`] entry.
async fn build_symlink_entry(
    fs_path: &Path,
    rel: PathBuf,
    mtime: i64,
    metadata: &Metadata,
    preserve: PreserveFlags,
) -> Result<Flist, DomainError> {
    let target = fs::read_link(fs_path).await.map_err(|e| {
        DomainError::RsyncProtocolError(format!("gen_flist: readlink({}): {e}", fs_path.display()))
    })?;
    let perm_bits = pick_mode(metadata, false, preserve) & !S_IFMT;
    Ok(Flist::symlink(
        rel,
        target,
        mtime,
        S_IFLNK | perm_bits,
        pick_uid(metadata, preserve),
        pick_gid(metadata, preserve),
    ))
}

/// Pick the wire mtime: the real on-disk mtime when `preserve.mtime`
/// is set, `0` otherwise (the rsync server still accepts a zeroed
/// mtime; the receiver simply doesn't update its tree's mtime field).
const fn pick_mtime(real: i64, preserve: PreserveFlags) -> i64 {
    if preserve.mtime { real } else { 0 }
}

/// Pick the wire mode: the real on-disk perm bits | the synthetic
/// type marker when `preserve.perms` is set, the synthetic default
/// otherwise. The synthetic default is `0o755` for directories and
/// `0o644` for everything else.
fn pick_mode(metadata: &Metadata, is_directory: bool, preserve: PreserveFlags) -> u32 {
    if preserve.perms {
        let real = real_mode(metadata);
        let kind = if is_directory { S_IFDIR } else { real & S_IFMT };
        return (real & !S_IFMT) | kind;
    }
    walk_mode_default(is_directory)
}

/// Pick the wire uid — real on-disk uid when `preserve.owner` is set,
/// `0` otherwise.
fn pick_uid(metadata: &Metadata, preserve: PreserveFlags) -> u32 {
    if preserve.owner {
        real_uid(metadata)
    } else {
        0
    }
}

/// Pick the wire gid — real on-disk gid when `preserve.group` is set,
/// `0` otherwise.
fn pick_gid(metadata: &Metadata, preserve: PreserveFlags) -> u32 {
    if preserve.group {
        real_gid(metadata)
    } else {
        0
    }
}

/// Pull the real on-disk mode bits off [`Metadata`]. UNIX-only — on
/// other platforms the function returns the synthetic regular-file
/// default (the rsync wire is fundamentally a POSIX protocol so the
/// fallback is "best effort").
#[cfg(unix)]
fn real_mode(metadata: &Metadata) -> u32 {
    metadata.mode()
}

#[cfg(not(unix))]
const fn real_mode(_metadata: &Metadata) -> u32 {
    walk_mode_default(false)
}

/// Pull the real on-disk uid off [`Metadata`]. UNIX-only.
#[cfg(unix)]
fn real_uid(metadata: &Metadata) -> u32 {
    metadata.uid()
}

#[cfg(not(unix))]
const fn real_uid(_metadata: &Metadata) -> u32 {
    0
}

/// Pull the real on-disk gid off [`Metadata`]. UNIX-only.
#[cfg(unix)]
fn real_gid(metadata: &Metadata) -> u32 {
    metadata.gid()
}

#[cfg(not(unix))]
const fn real_gid(_metadata: &Metadata) -> u32 {
    0
}

/// Pull the modification time off [`Metadata`] as unix seconds. Falls
/// back to `0` when the platform does not surface a modification time
/// (e.g. older WASI without atime support).
fn metadata_mtime(metadata: &Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

/// Default permission bits when the walker has no platform-specific
/// uid/gid context. Directories get `0o755`, regular files get `0o644`,
/// symlinks get `0o777` per POSIX convention.
const fn walk_mode_default(is_directory: bool) -> u32 {
    if is_directory {
        S_IFDIR | 0o755
    } else {
        S_IFREG | 0o644
    }
}

/// Sort entries lexicographically by their wire-relative path. Mirrors
/// `flist.c::flist_cmp` (line 67) which uses `strcmp` on `wpath`.
fn sort_entries(entries: &mut [Flist]) {
    // `sort_by_cached_key` computes `path_bytes()` once per element
    // instead of once per comparison — identical `Ord` on `Vec<u8>`
    // keys, so the resulting order is byte-for-byte unchanged.
    entries.sort_by_cached_key(Flist::path_bytes);
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::too_many_lines,
    reason = "test code uses unwrap/expect for brevity per project convention; longer table-driven tests document the FLIST_* matrix verbatim"
)]
mod tests {
    use super::{
        FLIST_GID_SAME, FLIST_MODE_SAME, FLIST_NAME_LONG, FLIST_NAME_SAME, FLIST_TIME_SAME,
        FLIST_TOP_LEVEL, FLIST_UID_SAME, Flist, FlistRecvOpts, FlistSendOpts, gen_flist_local,
        is_dir, is_lnk, is_reg, path_has_backtrack, path_to_posix_bytes, recv_flist, send_flist,
    };
    use crate::adapters::rsync::wire::io::{MplexReader, MplexWriter};
    use crate::adapters::rsync::wire::session::{RSYNC_PROTOCOL, WireSession};
    use std::path::PathBuf;
    use tokio::io::duplex;

    fn reg_entry(path: &str, size: i64) -> Flist {
        Flist::regular(PathBuf::from(path), size, 1_700_000_000, 0o644, 1000, 1000)
    }

    fn dir_entry(path: &str) -> Flist {
        Flist::directory(PathBuf::from(path), 1_700_000_000, 0o755, 1000, 1000)
    }

    fn lnk_entry(path: &str, target: &str) -> Flist {
        Flist::symlink(
            PathBuf::from(path),
            PathBuf::from(target),
            1_700_000_000,
            0o777,
            1000,
            1000,
        )
    }

    #[test]
    fn flist_flag_bits_match_openrsync() {
        // Verbatim copy of openrsync flist.c lines 55..62.
        assert_eq!(FLIST_TOP_LEVEL, 0x01);
        assert_eq!(FLIST_MODE_SAME, 0x02);
        assert_eq!(FLIST_UID_SAME, 0x08);
        assert_eq!(FLIST_GID_SAME, 0x10);
        assert_eq!(FLIST_NAME_SAME, 0x20);
        assert_eq!(FLIST_NAME_LONG, 0x40);
        assert_eq!(FLIST_TIME_SAME, 0x80);
    }

    #[test]
    fn mode_predicates_route_canonical_bits() {
        assert!(is_reg(0o100_644));
        assert!(is_dir(0o040_755));
        assert!(is_lnk(0o120_777));
        assert!(!is_reg(0o040_755));
        assert!(!is_dir(0o100_644));
        assert!(!is_lnk(0o100_644));
    }

    #[test]
    fn path_to_posix_bytes_replaces_backslashes() {
        let bytes = path_to_posix_bytes(&PathBuf::from("a\\b/c"));
        assert_eq!(bytes, b"a/b/c");
    }

    #[test]
    fn path_has_backtrack_table() {
        assert!(path_has_backtrack(".."));
        assert!(path_has_backtrack("../etc/passwd"));
        assert!(path_has_backtrack("a/../b"));
        assert!(path_has_backtrack("a/.."));
        assert!(!path_has_backtrack("a/b"));
        assert!(!path_has_backtrack("..tail"));
        assert!(!path_has_backtrack("a/..tail"));
    }

    #[tokio::test]
    async fn round_trip_two_regular_files() {
        let entries = vec![reg_entry("a.txt", 12), reg_entry("b.txt", 28)];
        let (left, right) = duplex(64 * 1024);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        send_flist(
            &mut w,
            &mut sess_w,
            &entries,
            FlistSendOpts::default(),
            RSYNC_PROTOCOL,
        )
        .await
        .expect("send");

        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        let got = recv_flist(
            &mut r,
            &mut sess_r,
            FlistRecvOpts::default(),
            RSYNC_PROTOCOL,
        )
        .await
        .expect("recv");
        assert_eq!(got.len(), 2);
        for (i, want) in entries.iter().enumerate() {
            assert_eq!(got[i].path, want.path);
            assert_eq!(got[i].size, want.size);
            assert_eq!(got[i].mtime, want.mtime);
            assert_eq!(got[i].mode, want.mode);
        }
    }

    #[tokio::test]
    async fn round_trip_directory_and_files() {
        let entries = vec![
            dir_entry("."),
            reg_entry("a.txt", 8),
            reg_entry("b.txt", 16),
        ];
        let (left, right) = duplex(64 * 1024);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        send_flist(
            &mut w,
            &mut sess_w,
            &entries,
            FlistSendOpts::default(),
            RSYNC_PROTOCOL,
        )
        .await
        .expect("send");

        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        let got = recv_flist(
            &mut r,
            &mut sess_r,
            FlistRecvOpts::default(),
            RSYNC_PROTOCOL,
        )
        .await
        .expect("recv");
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].path, PathBuf::from("."));
        assert!(is_dir(got[0].mode));
        assert!(is_reg(got[1].mode));
        assert!(is_reg(got[2].mode));
    }

    #[tokio::test]
    async fn round_trip_symlink_with_preserve_links() {
        let entries = vec![reg_entry("a.txt", 8), lnk_entry("link", "a.txt")];
        let (left, right) = duplex(64 * 1024);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let opts = FlistSendOpts {
            preserve_links: true,
            ..FlistSendOpts::default()
        };
        let recv_opts = FlistRecvOpts {
            preserve_links: true,
            ..FlistRecvOpts::default()
        };
        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        send_flist(&mut w, &mut sess_w, &entries, opts, RSYNC_PROTOCOL)
            .await
            .expect("send");
        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        let got = recv_flist(&mut r, &mut sess_r, recv_opts, RSYNC_PROTOCOL)
            .await
            .expect("recv");
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].link, Some(PathBuf::from("a.txt")));
        assert!(is_lnk(got[1].mode));
    }

    #[tokio::test]
    async fn round_trip_with_uid_gid_preservation() {
        let mut entry = reg_entry("file", 4);
        entry.uid = 4321;
        entry.gid = 1234;
        let entries = vec![entry.clone()];
        let (left, right) = duplex(64 * 1024);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let send_opts = FlistSendOpts {
            preserve_uids: true,
            preserve_gids: true,
            ..FlistSendOpts::default()
        };
        let recv_opts = FlistRecvOpts {
            preserve_uids: true,
            preserve_gids: true,
            ..FlistRecvOpts::default()
        };
        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        send_flist(&mut w, &mut sess_w, &entries, send_opts, RSYNC_PROTOCOL)
            .await
            .expect("send");
        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        let got = recv_flist(&mut r, &mut sess_r, recv_opts, RSYNC_PROTOCOL)
            .await
            .expect("recv");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].uid, 4321);
        assert_eq!(got[0].gid, 1234);
    }

    #[tokio::test]
    async fn empty_flist_round_trips_to_empty_vec() {
        let (left, right) = duplex(64);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        send_flist(
            &mut w,
            &mut sess_w,
            &[],
            FlistSendOpts::default(),
            RSYNC_PROTOCOL,
        )
        .await
        .expect("send");
        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        let got = recv_flist(
            &mut r,
            &mut sess_r,
            FlistRecvOpts::default(),
            RSYNC_PROTOCOL,
        )
        .await
        .expect("recv");
        assert!(got.is_empty());
    }

    #[tokio::test]
    async fn large_size_round_trips_via_long_varint() {
        let entries = vec![reg_entry("big.bin", 5_000_000_000)];
        let (left, right) = duplex(64 * 1024);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        send_flist(
            &mut w,
            &mut sess_w,
            &entries,
            FlistSendOpts::default(),
            RSYNC_PROTOCOL,
        )
        .await
        .expect("send");
        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        let got = recv_flist(
            &mut r,
            &mut sess_r,
            FlistRecvOpts::default(),
            RSYNC_PROTOCOL,
        )
        .await
        .expect("recv");
        assert_eq!(got[0].size, 5_000_000_000);
    }

    #[tokio::test]
    async fn top_level_flag_round_trips() {
        let mut entry = dir_entry(".");
        entry.flags = FLIST_TOP_LEVEL;
        let entries = vec![entry];
        let (left, right) = duplex(64 * 1024);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        send_flist(
            &mut w,
            &mut sess_w,
            &entries,
            FlistSendOpts::default(),
            RSYNC_PROTOCOL,
        )
        .await
        .expect("send");
        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        let got = recv_flist(
            &mut r,
            &mut sess_r,
            FlistRecvOpts::default(),
            RSYNC_PROTOCOL,
        )
        .await
        .expect("recv");
        assert_eq!(got[0].flags, FLIST_TOP_LEVEL);
    }

    #[tokio::test]
    async fn recv_rejects_absolute_pathname() {
        // Manually craft a flist whose first entry's pathname starts
        // with `/` — recv must reject as a security violation.
        let entries = vec![reg_entry("/etc/passwd", 1)];
        let (left, right) = duplex(64 * 1024);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        send_flist(
            &mut w,
            &mut sess_w,
            &entries,
            FlistSendOpts::default(),
            RSYNC_PROTOCOL,
        )
        .await
        .expect("send");
        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        let err = recv_flist(
            &mut r,
            &mut sess_r,
            FlistRecvOpts::default(),
            RSYNC_PROTOCOL,
        )
        .await
        .expect_err("must err");
        assert!(format!("{err}").contains("absolute"));
    }

    #[tokio::test]
    async fn recv_rejects_backtracking_pathname() {
        let entries = vec![reg_entry("../etc/passwd", 1)];
        let (left, right) = duplex(64 * 1024);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        send_flist(
            &mut w,
            &mut sess_w,
            &entries,
            FlistSendOpts::default(),
            RSYNC_PROTOCOL,
        )
        .await
        .expect("send");
        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        let err = recv_flist(
            &mut r,
            &mut sess_r,
            FlistRecvOpts::default(),
            RSYNC_PROTOCOL,
        )
        .await
        .expect_err("must err");
        assert!(format!("{err}").contains("backtrack"));
    }

    #[tokio::test]
    async fn send_total_size_accumulates_for_regular_files() {
        let entries = vec![reg_entry("a", 100), reg_entry("b", 200), dir_entry("c")];
        let (left, right) = duplex(64 * 1024);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        drop(rr);
        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        send_flist(
            &mut w,
            &mut sess_w,
            &entries,
            FlistSendOpts::default(),
            RSYNC_PROTOCOL,
        )
        .await
        .expect("send");
        // Directory does NOT contribute to total_size.
        assert_eq!(sess_w.total_size, 300);
    }

    #[tokio::test]
    async fn gen_flist_local_walks_simple_tree() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), b"hello").expect("a");
        std::fs::write(dir.path().join("b.txt"), b"world!").expect("b");
        std::fs::create_dir_all(dir.path().join("nested")).expect("mkdir");
        std::fs::write(dir.path().join("nested/c.txt"), b"deep").expect("c");

        let got = gen_flist_local(dir.path()).await.expect("walk");
        // Expected entries: ".", "a.txt", "b.txt", "nested",
        // "nested/c.txt" → 5 total.
        assert_eq!(got.len(), 5, "got entries: {got:#?}");
        // First entry is the top-level "." with FLIST_TOP_LEVEL set.
        let top = got
            .iter()
            .find(|e| e.path == PathBuf::from("."))
            .expect("top");
        assert_eq!(top.flags, FLIST_TOP_LEVEL);
        assert!(is_dir(top.mode));
        // a.txt is a regular 5-byte file.
        let a = got
            .iter()
            .find(|e| e.path == PathBuf::from("a.txt"))
            .expect("a");
        assert_eq!(a.size, 5);
        assert!(is_reg(a.mode));
        // nested/c.txt size = 4.
        let c = got
            .iter()
            .find(|e| e.path == PathBuf::from("nested/c.txt"))
            .expect("c");
        assert_eq!(c.size, 4);
    }

    #[tokio::test]
    async fn gen_flist_local_rejects_non_directory_root() {
        let file = tempfile::NamedTempFile::new().expect("tmp");
        let err = gen_flist_local(file.path()).await.expect_err("must err");
        assert!(format!("{err}").contains("not a directory"));
    }

    /// Slice 9 — when `preserve.perms` is on, the local walker copies
    /// the real on-disk perm bits onto the wire entry instead of the
    /// synthetic `0o755` / `0o644` defaults.
    #[cfg(unix)]
    #[tokio::test]
    async fn gen_flist_local_with_opts_preserves_real_mode_when_perms_set() {
        use super::gen_flist_local_with_opts;
        use crate::adapters::rsync::types::PreserveFlags;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("only_owner.txt");
        std::fs::write(&path, b"hi").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");

        let preserve = PreserveFlags {
            perms: true,
            mtime: false,
            owner: false,
            group: false,
            links: false,
            hardlinks: false,
            sparse: false,
            devices: false,
        };
        let got = gen_flist_local_with_opts(dir.path(), preserve)
            .await
            .expect("walk");
        let entry = got
            .iter()
            .find(|e| e.path == PathBuf::from("only_owner.txt"))
            .expect("entry");
        assert_eq!(entry.mode & 0o7777, 0o600);
        assert!(is_reg(entry.mode));
    }

    /// Slice 9 — when `preserve.mtime` is off, the local walker zeroes
    /// the mtime field so the rsync server never pins a fingerprint
    /// against it.
    #[tokio::test]
    async fn gen_flist_local_with_opts_zeroes_mtime_when_preserve_unset() {
        use super::gen_flist_local_with_opts;
        use crate::adapters::rsync::types::PreserveFlags;

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), b"hi").expect("write");
        let preserve = PreserveFlags::none();
        let got = gen_flist_local_with_opts(dir.path(), preserve)
            .await
            .expect("walk");
        let entry = got
            .iter()
            .find(|e| e.path == PathBuf::from("a.txt"))
            .expect("entry");
        assert_eq!(entry.mtime, 0);
    }

    /// Bug-B regression — exclude pattern drops matching files from the
    /// local flist before send.
    #[tokio::test]
    async fn gen_flist_local_with_filters_drops_excluded_files() {
        use super::{FlistFilters, gen_flist_local_with_filters};
        use crate::adapters::rsync::sftp::walker::build_globset;
        use crate::adapters::rsync::types::PreserveFlags;

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("keep.txt"), b"keep").expect("write");
        std::fs::write(dir.path().join("drop.tmp"), b"drop").expect("write");
        let excludes = build_globset(&["*.tmp".to_string()]).expect("globset");
        let includes = build_globset(&[]).expect("globset");
        let filters = FlistFilters {
            excludes: &excludes,
            includes: &includes,
        };
        let got = gen_flist_local_with_filters(dir.path(), PreserveFlags::none(), Some(&filters))
            .await
            .expect("walk");
        let names: Vec<&str> = got.iter().filter_map(|e| e.path.to_str()).collect();
        assert!(names.contains(&"keep.txt"));
        assert!(!names.contains(&"drop.tmp"));
    }

    /// Bug-B regression — non-empty include set rescues a matching
    /// exclude.
    #[tokio::test]
    async fn gen_flist_local_with_filters_include_overrides_exclude() {
        use super::{FlistFilters, gen_flist_local_with_filters};
        use crate::adapters::rsync::sftp::walker::build_globset;
        use crate::adapters::rsync::types::PreserveFlags;

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.tmp"), b"a").expect("write");
        std::fs::write(dir.path().join("b.tmp"), b"b").expect("write");
        let excludes = build_globset(&["*.tmp".to_string()]).expect("excludes");
        let includes = build_globset(&["a.tmp".to_string()]).expect("includes");
        let filters = FlistFilters {
            excludes: &excludes,
            includes: &includes,
        };
        let got = gen_flist_local_with_filters(dir.path(), PreserveFlags::none(), Some(&filters))
            .await
            .expect("walk");
        let names: Vec<&str> = got.iter().filter_map(|e| e.path.to_str()).collect();
        assert!(names.contains(&"a.tmp"));
        assert!(!names.contains(&"b.tmp"));
    }

    /// Bug-B regression — `None` filters preserves the legacy walk
    /// shape so v6.x callers keep emitting every file.
    #[tokio::test]
    async fn gen_flist_local_with_filters_none_keeps_all_entries() {
        use super::gen_flist_local_with_filters;
        use crate::adapters::rsync::types::PreserveFlags;

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.tmp"), b"a").expect("write");
        std::fs::write(dir.path().join("b.txt"), b"b").expect("write");
        let got = gen_flist_local_with_filters(dir.path(), PreserveFlags::none(), None)
            .await
            .expect("walk");
        let names: Vec<&str> = got.iter().filter_map(|e| e.path.to_str()).collect();
        assert!(names.contains(&"a.tmp"));
        assert!(names.contains(&"b.txt"));
    }

    // ============================================================
    // Slice-4 wire-shape tests — proto-28+ XMIT_EXTENDED_FLAGS
    // and proto-30+ varint30 / varlong30 length / size / mtime fields.
    // ============================================================

    use super::{
        XMIT_EXTENDED_FLAGS, XMIT_LONG_NAME_16, XMIT_TOP_DIR_16, encode_varint, encode_varlong,
    };
    use tokio::io::AsyncReadExt;

    #[test]
    fn xmit_extended_flag_constants_match_upstream_rsync() {
        // Upstream rsync 3.2.7 rsync.h:
        //   #define XMIT_EXTENDED_FLAGS (1<<2)  /* protocols 28 - now */
        //   #define XMIT_TOP_DIR        (1<<0)
        //   #define XMIT_LONG_NAME      (1<<6)
        assert_eq!(XMIT_EXTENDED_FLAGS, 1 << 2);
        assert_eq!(XMIT_TOP_DIR_16, 1 << 0);
        assert_eq!(XMIT_LONG_NAME_16, 1 << 6);
    }

    #[test]
    fn varint_encoder_matches_upstream_byte_table() {
        // Reference values verified against upstream rsync 3.2.7
        // io.c::write_varint by hand. The 1-byte table covers 0..=127.
        let (buf, cnt) = encode_varint(0);
        assert_eq!(&buf[..cnt], &[0]);
        let (buf, cnt) = encode_varint(1);
        assert_eq!(&buf[..cnt], &[1]);
        let (buf, cnt) = encode_varint(0x7f);
        assert_eq!(&buf[..cnt], &[0x7f]);
        // 128 -> two bytes: prefix 0x80, then payload 0x80.
        let (buf, cnt) = encode_varint(128);
        assert_eq!(&buf[..cnt], &[0x80, 0x80]);
        // 0x4000 -> three bytes: prefix 0xc0, payload 0x00 0x40.
        let (buf, cnt) = encode_varint(0x4000);
        assert_eq!(&buf[..cnt], &[0xc0, 0x00, 0x40]);
    }

    #[test]
    fn varlong_encoder_matches_upstream_byte_table() {
        // min_bytes=3 (file-size encoding) — emits 3 bytes for values
        // that fit in 23 bits.
        let (buf, cnt) = encode_varlong(0, 3);
        assert_eq!(&buf[..cnt], &[0, 0, 0]);
        let (buf, cnt) = encode_varlong(1, 3);
        assert_eq!(&buf[..cnt], &[0, 1, 0]);
        // 100 fits in 7 bits with min_bytes=3: prefix=0x00 (low 6 bits
        // of head become payload byte 1).
        let (buf, cnt) = encode_varlong(100, 3);
        assert_eq!(&buf[..cnt], &[0, 100, 0]);
    }

    #[tokio::test]
    async fn varint_round_trip_table() {
        for &val in &[
            0_i32,
            1,
            127,
            128,
            255,
            256,
            16_383,
            16_384,
            65_535,
            65_536,
            1_000_000,
            i32::MAX,
        ] {
            let (left, right) = duplex(64);
            let (lr, lw) = tokio::io::split(left);
            let (rr, _rw) = tokio::io::split(right);
            drop(lr);
            let mut w = MplexWriter::new(lw);
            let mut sess_w = WireSession::new();
            super::write_varint(&mut w, &mut sess_w, val)
                .await
                .expect("write_varint");
            let mut r = MplexReader::new(rr);
            let mut sess_r = WireSession::new();
            let got = super::read_varint(&mut r, &mut sess_r)
                .await
                .expect("read_varint");
            assert_eq!(got, val, "varint round-trip failed for {val}");
        }
    }

    #[tokio::test]
    async fn varlong_round_trip_table() {
        for &val in &[
            0_i64,
            1,
            127,
            128,
            255,
            i64::from(i32::MAX),
            5_000_000_000,
            1_000_000_000_000,
        ] {
            let (left, right) = duplex(64);
            let (lr, lw) = tokio::io::split(left);
            let (rr, _rw) = tokio::io::split(right);
            drop(lr);
            let mut w = MplexWriter::new(lw);
            let mut sess_w = WireSession::new();
            super::write_varlong(&mut w, &mut sess_w, val, 3)
                .await
                .expect("write_varlong");
            let mut r = MplexReader::new(rr);
            let mut sess_r = WireSession::new();
            let got = super::read_varlong(&mut r, &mut sess_r, 3)
                .await
                .expect("read_varlong");
            assert_eq!(got, val, "varlong(3) round-trip failed for {val}");
        }
    }

    #[tokio::test]
    async fn proto31_top_level_dot_entry_matches_canonical_byte_shape() {
        // Single top-level "." directory entry at protocol 31:
        //
        //   flag short (LE, low byte first):
        //     XMIT_TOP_DIR | XMIT_LONG_NAME | XMIT_EXTENDED_FLAGS
        //     = 0x01 | 0x40 | 0x04 = 0x45 0x00
        //   varint30 path-length: 1 -> single byte 0x01
        //   path bytes: "."
        //   varlong(_, 3) size: 0 -> three bytes 0x00 0x00 0x00
        //   varlong(_, 4) mtime: 1_700_000_000 -> 0x00 prefix + 4 bytes
        //     (varlong puts the prefix's payload trailing).
        //   write_int mode: 0o040755 (4 bytes LE)
        //   end-of-list sentinel: 0x00
        let mut entry = dir_entry(".");
        entry.flags = FLIST_TOP_LEVEL;
        let entries = vec![entry];
        let (left, right) = duplex(64 * 1024);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        super::send_flist(&mut w, &mut sess_w, &entries, FlistSendOpts::default(), 31)
            .await
            .expect("send");
        // Read the canonical 4-byte prefix to assert the flag short +
        // varint name length + path byte. Avoid reading the trailing
        // varlong / mtime / mode / sentinel because their lengths
        // depend on the exact mtime varlong encoding and we don't
        // want to recompute them here — the round-trip tests above
        // already prove byte-equivalence end-to-end.
        let mut reader = rr;
        let mut prefix = [0_u8; 4];
        reader.read_exact(&mut prefix).await.expect("prefix");
        // Flag short: low byte = 0x45 (TOP_DIR | LONG_NAME | EXTENDED_FLAGS).
        assert_eq!(prefix[0], 0x45);
        // Flag short: high byte = 0x00.
        assert_eq!(prefix[1], 0x00);
        // Varint path-length = 1 (single byte, fits in 7 bits).
        assert_eq!(prefix[2], 0x01);
        // Path byte ".".
        assert_eq!(prefix[3], b'.');
    }

    #[tokio::test]
    async fn proto31_round_trip_regular_file_matches_size_via_varlong() {
        // Confirm a regular file > 65535 bytes round-trips at protocol
        // 31 — exercises the varlong3 encoder for a non-trivial value.
        let entries = vec![reg_entry("big.bin", 5_000_000_000)];
        let (left, right) = duplex(64 * 1024);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        super::send_flist(&mut w, &mut sess_w, &entries, FlistSendOpts::default(), 31)
            .await
            .expect("send");
        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        let got = super::recv_flist(&mut r, &mut sess_r, FlistRecvOpts::default(), 31)
            .await
            .expect("recv");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, PathBuf::from("big.bin"));
        assert_eq!(got[0].size, 5_000_000_000);
    }

    #[tokio::test]
    async fn proto27_round_trip_keeps_legacy_8bit_flag_shape() {
        // The legacy openrsync wire shape stays available for any
        // call site that hands a proto<28 negotiated value.
        let entries = vec![reg_entry("a.txt", 12)];
        let (left, right) = duplex(64 * 1024);
        let (lr, lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        let mut w = MplexWriter::new(lw);
        let mut sess_w = WireSession::new();
        super::send_flist(&mut w, &mut sess_w, &entries, FlistSendOpts::default(), 27)
            .await
            .expect("send");
        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        let got = super::recv_flist(&mut r, &mut sess_r, FlistRecvOpts::default(), 27)
            .await
            .expect("recv");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, PathBuf::from("a.txt"));
        assert_eq!(got[0].size, 12);
    }

    /// Slice-8 regression — captured from rsync 3.2.7 against a real VM
    /// during a `--server --sender` pull session. The first entry's
    /// `xflags=0x201d` carries `XMIT_MOD_NSEC` (`1<<13`), which
    /// pre-slice-8 our decoder ignored — that mis-aligned the entire
    /// downstream byte stream and produced spurious "non-utf8 pathname"
    /// errors a couple of entries later. The captured bytes here are
    /// the exact wire representation of one directory entry; the test
    /// asserts the decoder consumes the `MOD_NSEC` varint suffix and
    /// produces a clean entry instead of swallowing the next entry's
    /// flag byte.
    #[tokio::test]
    async fn recv_flist_consumes_xmit_mod_nsec_at_proto31() {
        // xflags low=0x1d (TOP_DIR | EXTENDED_FLAGS | SAME_UID | SAME_GID),
        // high=0x20 (MOD_NSEC bit, `1<<13` >> 8 == 0x20).
        // No SAME_NAME -> no l1 byte.
        // No LONG_NAME -> l2 = 5 bytes -> "hello".
        // size varlong3 -> [0x00, 0x00, 0x00] = 0.
        // mtime varlong4 -> [0x00, 0x00, 0x00, 0x00] = 0.
        // MOD_NSEC varint -> [0x00] = 0 (1-byte form).
        // mode int32 LE -> 0x000041ed (drwxr-xr-x).
        // Flag byte -> 0x00 (end of list).
        let mut bytes: Vec<u8> = Vec::new();
        bytes.push(0x1d);
        bytes.push(0x20);
        bytes.push(5);
        bytes.extend_from_slice(b"hello");
        bytes.extend_from_slice(&[0x00, 0x00, 0x00]);
        bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        bytes.push(0x00); // MOD_NSEC varint = 0
        bytes.extend_from_slice(&0x0000_41ed_u32.to_le_bytes());
        bytes.push(0x00); // end-of-list

        let (left, right) = duplex(64 * 1024);
        let (lr, mut lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        tokio::io::AsyncWriteExt::write_all(&mut lw, &bytes)
            .await
            .expect("write fixture");
        drop(lw);

        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        let entries = super::recv_flist(&mut r, &mut sess_r, FlistRecvOpts::default(), 31)
            .await
            .expect("recv_flist");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, PathBuf::from("hello"));
        assert_eq!(entries[0].size, 0);
        assert_eq!(entries[0].mode, 0x0000_41ed);
    }

    /// Slice-8 — the proto-31+ end-of-list `io_error` sentinel
    /// (`XMIT_EXTENDED_FLAGS | XMIT_IO_ERROR_ENDLIST` = `0x1004`)
    /// terminates the receive loop without emitting a stray entry.
    /// Mirrors rsync 3.2.7's `flist.c::recv_file_list` line 2631.
    #[tokio::test]
    async fn recv_flist_treats_io_error_endlist_as_terminator() {
        // First and only frame: low=0x04 (EXTENDED_FLAGS, non-zero so
        // the "first byte == 0" early-out is skipped), high=0x10
        // (IO_ERROR_ENDLIST = 1<<12 -> 0x10 in the high byte). The
        // payload is a 1-byte varint reporting err=0.
        let bytes: Vec<u8> = vec![0x04, 0x10, 0x00];
        let (left, right) = duplex(64 * 1024);
        let (lr, mut lw) = tokio::io::split(left);
        let (rr, _rw) = tokio::io::split(right);
        drop(lr);
        tokio::io::AsyncWriteExt::write_all(&mut lw, &bytes)
            .await
            .expect("write fixture");
        drop(lw);

        let mut r = MplexReader::new(rr);
        let mut sess_r = WireSession::new();
        let entries = super::recv_flist(&mut r, &mut sess_r, FlistRecvOpts::default(), 31)
            .await
            .expect("recv_flist");
        assert!(entries.is_empty());
    }
}
