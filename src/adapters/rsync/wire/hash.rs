// SPDX-License-Identifier: ISC
//! Ported from OpenBSD's openrsync — `hash.c` + `md4.c`.
//!
//! Original copyright: Kristaps Dzonsons; ISC license. See
//! `LICENSES/openrsync-ISC.txt` for the full notice.
//!
//! # Scope (v7.0.0-alpha.X)
//!
//! - [`hash_fast`] — port of `hash.c::hash_fast` (lines 32..60).
//!   Tridgell-style rolling 32-bit hash. Used by both sender (whole
//!   buffer hash for matching) and the future receiver.
//! - [`hash_slow_into`] — port of `hash.c::hash_slow` (lines 65..76).
//!   MD4 of `(buf || seed_le)` truncated by the caller per
//!   `BlockSet::strong_len`.
//! - [`FileHasher`] — port of upstream rsync 3.2.7's
//!   `checksum.c::sum_init`/`sum_update`/`sum_end` triplet (lines
//!   556..720). Drives the per-file transfer digest the receiver
//!   verifies in `receiver.c::receive_data` line 411.
//!
//! ## Per-file digest selection
//!
//! Upstream rsync 3.2.7's `parse_csum_name` in `checksum.c::109..143`
//! switches the per-file `xfer_sum_nni` based on the negotiated
//! protocol version + the `do_negotiated_strings` compat flag:
//!
//! - **protocol < 30** — implied `CSUM_MD4_OLD` =
//!   `MD4(seed_le || file_bytes)` truncated to `MD4_DIGEST_LEN` (16).
//!   This is what openrsync emits because it pins protocol 27.
//! - **protocol >= 30** — implied `CSUM_MD5` =
//!   `MD5(file_bytes)` (no seed; the per-block strong checksum is
//!   the only place the seed continues to be folded in). 16 bytes.
//!
//! [`FileHasher`] is an enum that carries the right algorithm for the
//! negotiated protocol. The sender state machine picks the variant via
//! [`FileHasher::for_protocol`]. Slice 5 wires the MD5 path because
//! every modern rsync server (3.x) negotiates protocol 30 or 31 and
//! the receiver's `xfer_sum_nni` defaults to MD5 in that band.
//!
//! MD4 is delegated to the `md4` crate; MD5 to the `md-5` crate. Both
//! are part of the `RustCrypto` family and the standard choice across
//! the Rust ecosystem — saves us re-porting `md4.c` / `md5.c`.
//!
//! # Lock-free contract (CRITICAL)
//!
//! All state is per-call or per-task local. [`FileHasher`] owns its
//! digest context exclusively; the sender state machine threads it
//! through `&mut FileHasher`. No `Mutex`, no shared counter.

use md4::{Digest as Md4Digest, Md4};
use md5::{Digest as Md5Digest, Md5};

use crate::adapters::rsync::wire::session::VARINT_FLIST_MIN_PROTOCOL;

/// Tridgell-style 32-bit rolling hash. Direct port of openrsync's
/// `hash.c::hash_fast` (lines 32..60).
///
/// The C original treats input bytes as `signed char` (sign-extending
/// each byte to the accumulator). The faithful Rust translation casts
/// each `u8` through `i32` via `i32::from(byte as i8)`, which carries
/// the same sign extension. **Do not** use `i32::from(byte)` directly:
/// that path zero-extends and produces a different hash modulo
/// `0xffff` for any input byte with the high bit set.
#[must_use]
pub fn hash_fast(buf: &[u8]) -> u32 {
    // a is a(k, l) accumulator; b is b(k, l) accumulator (rsync paper
    // notation). The expression `(b << 16)` lives in u32 wrap-around
    // arithmetic to mirror C's implicit `uint32_t` width.
    let len = buf.len();
    let mut a = 0_u32;
    let mut b = 0_u32;
    let mut i = 0_usize;

    if len > 4 {
        let stop = len - 4;
        while i < stop {
            hash_fast_block_of_four(buf, i, &mut a, &mut b);
            i = i.saturating_add(4);
        }
    }

    while i < len {
        let bi = sign_extended(buf, i);
        a = a.wrapping_add(bi);
        b = b.wrapping_add(a);
        i = i.saturating_add(1);
    }

    (a & 0xffff).wrapping_add(b << 16_u32)
}

/// Process one 4-byte block of the SIMD-style fast-hash inner loop.
/// Mirrors the body of openrsync's `hash.c::hash_fast` for-loop (lines
/// 41..50): four sign-extended bytes folded into both accumulators.
fn hash_fast_block_of_four(buf: &[u8], i: usize, a: &mut u32, b: &mut u32) {
    let b0 = sign_extended(buf, i);
    let b1 = sign_extended(buf, i.saturating_add(1));
    let b2 = sign_extended(buf, i.saturating_add(2));
    let b3 = sign_extended(buf, i.saturating_add(3));
    // openrsync: b += 4 * (a + dat[i]) + 3 * dat[i+1] + 2 * dat[i+2] + dat[i+3];
    *b = b
        .wrapping_add(a.wrapping_add(b0).wrapping_mul(4))
        .wrapping_add(b1.wrapping_mul(3))
        .wrapping_add(b2.wrapping_mul(2))
        .wrapping_add(b3);
    *a = a
        .wrapping_add(b0)
        .wrapping_add(b1)
        .wrapping_add(b2)
        .wrapping_add(b3);
}

/// Read `buf[i]` as a sign-extended `u32`. Mirrors C's
/// `(int32_t)(signed char)dat[i]` pattern (where `dat` is `signed
/// char *`).
fn sign_extended(buf: &[u8], i: usize) -> u32 {
    let byte = buf.get(i).copied().unwrap_or(0);
    sign_extend_byte(byte)
}

/// Sign-extend a `u8` into a `u32` carrying the same bit pattern as
/// C's `(uint32_t)(int32_t)(signed char)b`.
///
/// Exposed because the rolling-window slide in [`hash_roll`] needs
/// to widen the byte being removed and the byte being added with the
/// same convention [`hash_fast`] already uses.
#[must_use]
pub fn sign_extend_byte(byte: u8) -> u32 {
    // Convert u8 -> i8 (preserves bit pattern) -> i32 (sign-extends) ->
    // u32 (preserves bit pattern). The double bit-cast is the canonical
    // way to express the C-style signed widening.
    let signed_byte = i8::from_ne_bytes([byte]);
    let widened = i32::from(signed_byte);
    u32::from_ne_bytes(widened.to_ne_bytes())
}

/// Compute the initial `(s1, s2)` partial-sum pair for the rolling
/// hash.
///
/// `s1` is the low 16-bit accumulator; `s2` is the high 16-bit
/// accumulator. Together they recover [`hash_fast`]'s output via
/// [`combine_s1_s2`].
///
/// Mirrors openrsync's `blk_find` recomp branch at `blocks.c` lines
/// 161..164: it calls `hash_fast`, then splits the returned `u32`
/// back into `(s1 = h & 0xFFFF, s2 = h >> 16)`. We expose the split
/// directly so the matcher does not have to round-trip through
/// `hash_fast`'s `(a & 0xffff) + (b << 16)` packing.
#[must_use]
pub fn hash_split(buf: &[u8]) -> (u32, u32) {
    let h = hash_fast(buf);
    (h & 0xffff, h >> 16_u32)
}

/// Combine `(s1, s2)` partial sums back into the packed 32-bit
/// rolling hash. Mirrors openrsync's `blk_find` line 159 (`fhash =
/// (st->s1 & 0xFFFF) | (st->s2 << 16);`).
#[must_use]
pub const fn combine_s1_s2(s1: u32, s2: u32) -> u32 {
    (s1 & 0xffff_u32) | (s2 << 16_u32)
}

/// Slide the rolling-hash window by one byte.
///
/// Direct port of the partial-sum adjustment block at openrsync
/// `blocks.c` lines 226..232:
///
/// ```c
/// map = st->map + st->offs;
/// st->s1 -= map[0];
/// st->s2 -= osz * map[0];
/// if (osz < remain) {
///     st->s1 += map[osz];
///     st->s2 += st->s1;
/// }
/// ```
///
/// `out_byte` is the byte leaving the window (was at offset 0);
/// `in_byte` is the byte entering the window (will live at offset
/// `block_len - 1` after the slide). When the window has reached the
/// end of the buffer the caller passes `None` for `in_byte` so we
/// only run the "remove" branch — mirrors the `osz < remain` guard.
///
/// `block_len` is the **current** block size (the `osz` in the C
/// source). On the last partial block this is `remainder` rather
/// than the full `block_len`; the caller picks the right value.
///
/// Returns the new `(s1, s2)` pair.
#[must_use]
pub fn hash_roll(
    s1: u32,
    s2: u32,
    out_byte: u8,
    in_byte: Option<u8>,
    block_len: u32,
) -> (u32, u32) {
    let out_widened = sign_extend_byte(out_byte);
    let mut new_s1 = s1.wrapping_sub(out_widened);
    let mut new_s2 = s2.wrapping_sub(block_len.wrapping_mul(out_widened));
    if let Some(byte_in) = in_byte {
        let in_widened = sign_extend_byte(byte_in);
        new_s1 = new_s1.wrapping_add(in_widened);
        new_s2 = new_s2.wrapping_add(new_s1);
    }
    (new_s1, new_s2)
}

/// Slow (strong) per-block MD4 with the session seed appended.
///
/// Direct port of openrsync's `hash.c::hash_slow` (lines 65..76):
///
/// ```c
/// MD4_Init(&ctx);
/// MD4_Update(&ctx, buf, len);
/// MD4_Update(&ctx, (unsigned char *)&seed, sizeof(int32_t));
/// MD4_Final(md, &ctx);
/// ```
///
/// The seed is hashed in **little-endian** byte order regardless of
/// host endianness — `htole32(sess->seed)` in the C source. We mirror
/// that with `seed.to_le_bytes()`.
///
/// Writes a 16-byte digest into `out`. Callers truncate to
/// `BlockSet::strong_len` bytes when comparing.
pub fn hash_slow_into(buf: &[u8], seed: i32, out: &mut [u8; 16]) {
    let mut hasher = Md4::new();
    hasher.update(buf);
    hasher.update(seed.to_le_bytes());
    let digest = hasher.finalize();
    out.copy_from_slice(&digest);
}

/// Streaming per-file transfer-digest hasher.
///
/// Mirrors the algorithm-switching `xfer_sum_nni` chain in upstream
/// rsync 3.2.7's `checksum.c::sum_init`/`sum_update`/`sum_end` (lines
/// 556..720). The per-file digest is the trailer the sender writes
/// immediately after the EOF token; the receiver verifies it in
/// `receiver.c::receive_data` line 411 — a mismatch returns
/// `recv_ok = 0` and the receiver discards / requests a redo.
///
/// Two algorithm variants live behind one type:
///
/// - [`FileHasher::Md4Seeded`] — `MD4(seed_le || file_bytes)` (16
///   bytes). Emitted at protocol < 30 to match the `CSUM_MD4_OLD`
///   default in upstream rsync. Direct port of openrsync's
///   `hash.c::hash_file_*` triplet (lines 83..101).
/// - [`FileHasher::Md5Plain`] — `MD5(file_bytes)` (16 bytes). Emitted
///   at protocol >= 30 to match the `CSUM_MD5` default. The seed is
///   intentionally not folded in: see `checksum.c::sum_init` lines
///   597..599 (the `CSUM_MD5` arm only calls `md5_begin(&ctx_md)`,
///   no `SIVAL` + `sum_update(s, 4)` prologue).
///
/// Both variants finalise to a fixed 16-byte digest so callers can
/// keep treating the trailer as a `[u8; 16]`.
#[derive(Debug)]
pub enum FileHasher {
    /// `MD4(seed_le || file_bytes)` — proto < 30 / `CSUM_MD4_OLD`.
    Md4Seeded(Md4),
    /// `MD5(file_bytes)` — proto >= 30 / `CSUM_MD5`.
    Md5Plain(Md5),
}

impl FileHasher {
    /// Pick the right algorithm for the negotiated protocol.
    ///
    /// At protocol >= [`VARINT_FLIST_MIN_PROTOCOL`] (= 30) — `CSUM_MD5`,
    /// no seed prefix. Below that — legacy `CSUM_MD4_OLD` with the
    /// `MD4(seed_le || ...)` shape openrsync emits.
    #[must_use]
    pub fn for_protocol(seed: i32, negotiated: i32) -> Self {
        if negotiated >= VARINT_FLIST_MIN_PROTOCOL {
            Self::Md5Plain(Md5::new())
        } else {
            let mut ctx = Md4::new();
            ctx.update(seed.to_le_bytes());
            Self::Md4Seeded(ctx)
        }
    }

    /// Initialise the legacy MD4-with-seed hasher unconditionally.
    /// Retained for callers (and tests) that want the openrsync
    /// proto-27 shape regardless of negotiated protocol.
    ///
    /// Mirrors `hash_file_start` from openrsync (`hash.c` lines
    /// 83..90).
    #[must_use]
    pub fn start(seed: i32) -> Self {
        let mut ctx = Md4::new();
        ctx.update(seed.to_le_bytes());
        Self::Md4Seeded(ctx)
    }

    /// Absorb a buffer of file bytes. Mirrors `hash_file_buf` (MD4
    /// variant) and `md5_update` (MD5 variant).
    pub fn update(&mut self, buf: &[u8]) {
        match self {
            Self::Md4Seeded(ctx) => Md4Digest::update(ctx, buf),
            Self::Md5Plain(ctx) => Md5Digest::update(ctx, buf),
        }
    }

    /// Finalise the hasher and return the 16-byte digest. Both MD4
    /// and MD5 produce 16-byte outputs.
    #[must_use]
    pub fn finish(self) -> [u8; 16] {
        let mut out = [0_u8; 16];
        match self {
            Self::Md4Seeded(ctx) => {
                let digest = ctx.finalize();
                out.copy_from_slice(&digest);
            }
            Self::Md5Plain(ctx) => {
                let digest = ctx.finalize();
                out.copy_from_slice(&digest);
            }
        }
        out
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test code uses unwrap/expect for brevity per project convention"
)]
mod tests {
    use super::{FileHasher, hash_fast, hash_slow_into};

    #[test]
    fn hash_fast_empty_buffer_is_zero() {
        assert_eq!(hash_fast(&[]), 0);
    }

    #[test]
    fn hash_fast_single_byte_zero_is_zero() {
        assert_eq!(hash_fast(&[0_u8]), 0);
    }

    #[test]
    fn hash_fast_single_byte_one() {
        // a = 1, b = 1 → return (1 & 0xffff) + (1 << 16) = 0x10001.
        assert_eq!(hash_fast(&[1_u8]), 0x0001_0001);
    }

    #[test]
    fn hash_fast_known_short_buffer() {
        // For len <= 4 the SIMD-style block path is skipped; the result
        // exercises only the trailing per-byte loop. Used as a
        // regression anchor against the openrsync reference.
        // For "rsync" (5 bytes) the loop sets:
        //   i=0: a=0+'r'=114, b=114
        //   ... last loop adds 'c' (last byte index 4) — but len=5
        //   means the SIMD loop runs once (i=0 covers bytes 0..4),
        //   the trailing loop processes byte 4 ('c').
        // The tail-only run (5 bytes "rsync"):
        //   Block path covers 'r','s','y','n':
        //     a = 114+115+121+110 = 460
        //     b = 4*(0+114) + 3*115 + 2*121 + 110 = 456 + 345 + 242 + 110 = 1153
        //   Trailing: i=4, byte 'c'=99
        //     a = 460+99 = 559
        //     b = 1153+559 = 1712
        //   final = (559 & 0xffff) + (1712 << 16) = 559 + 0x06b0_0000
        let h = hash_fast(b"rsync");
        let expected = 559_u32.wrapping_add(1712_u32 << 16_u32);
        assert_eq!(h, expected);
    }

    #[test]
    fn hash_fast_handles_high_bit_bytes_with_sign_extension() {
        // A byte with the high bit set sign-extends to a negative i32
        // in the openrsync implementation. `0xff` becomes `-1`. Verify
        // we produce the same result as the C reference: hash_fast([0xff]).
        // a += -1 → 0xffff_ffff
        // b += a → 0xffff_ffff
        // result = (0xffff_ffff & 0xffff) + (0xffff_ffff << 16)
        //        = 0xffff + 0xffff_0000
        //        = 0xffff_ffff
        let h = hash_fast(&[0xff_u8]);
        assert_eq!(h, 0xffff_ffff);
    }

    #[test]
    fn hash_fast_block_path_matches_tail_path() {
        // For a 4-byte buffer, the block path is skipped (len > 4 is
        // false), and only the tail loop runs. Confirm the output is
        // deterministic and matches a hand-computed reference.
        // For [1, 2, 3, 4] processed by tail loop:
        //   i=0: a=1, b=1
        //   i=1: a=3, b=4
        //   i=2: a=6, b=10
        //   i=3: a=10, b=20
        //   result = (10 & 0xffff) + (20 << 16) = 10 + 0x14_0000
        let h = hash_fast(&[1, 2, 3, 4]);
        assert_eq!(h, 10_u32.wrapping_add(20_u32 << 16_u32));
    }

    #[test]
    fn hash_slow_into_writes_16_bytes() {
        let mut out = [0_u8; 16];
        hash_slow_into(b"hello", 0, &mut out);
        // MD4 of any input is 16 bytes; ensure we touched the slot
        // (the all-zero result would only occur for a vanishingly rare
        // collision so this is a robust smoke check).
        assert_ne!(out, [0_u8; 16]);
    }

    #[test]
    fn hash_slow_into_is_seed_dependent() {
        let mut a = [0_u8; 16];
        let mut b = [0_u8; 16];
        hash_slow_into(b"payload", 1, &mut a);
        hash_slow_into(b"payload", 2, &mut b);
        assert_ne!(a, b, "different seeds must produce different digests");
    }

    #[test]
    fn hash_slow_into_seed_is_le_bytes() {
        // Verify the seed is absorbed in little-endian order — for
        // seed = 0x0001_0203, the four trailing bytes are 03 02 01 00.
        // Compute the same digest via the streaming API to confirm.
        use md4::{Digest, Md4};
        let mut native = [0_u8; 16];
        hash_slow_into(b"x", 0x0001_0203_i32, &mut native);

        let mut h = Md4::new();
        h.update(b"x");
        h.update([0x03_u8, 0x02, 0x01, 0x00]);
        let manual = h.finalize();
        assert_eq!(&native[..], manual.as_slice());
    }

    #[test]
    fn file_hasher_round_trip() {
        let mut h = FileHasher::start(42);
        h.update(b"hello ");
        h.update(b"world");
        let digest = h.finish();

        // Compare against a direct MD4 of (seed_le || "hello world").
        use md4::{Digest, Md4};
        let mut want = Md4::new();
        want.update(42_i32.to_le_bytes());
        want.update(b"hello world");
        assert_eq!(&digest[..], want.finalize().as_slice());
    }

    #[test]
    fn file_hasher_seed_zero_matches_streaming_md4_with_zero_prefix() {
        let mut h = FileHasher::start(0);
        h.update(b"");
        let got = h.finish();
        use md4::{Digest, Md4};
        let mut want = Md4::new();
        want.update([0_u8; 4]); // seed = 0 LE
        let want_d = want.finalize();
        assert_eq!(&got[..], want_d.as_slice());
    }

    #[test]
    fn file_hasher_for_protocol_31_uses_md5_no_seed() {
        // At negotiated >= 30 the per-file transfer digest is plain
        // MD5(file_bytes). No seed is folded in. Mirrors upstream rsync
        // 3.2.7 `checksum.c::sum_init` CSUM_MD5 arm.
        let mut h = FileHasher::for_protocol(0xdead_beef_u32 as i32, 31);
        h.update(b"hello world");
        let got = h.finish();

        use md5::{Digest as Md5Digest, Md5};
        let mut want = Md5::new();
        Md5Digest::update(&mut want, b"hello world");
        let want_d = want.finalize();
        assert_eq!(&got[..], want_d.as_slice());
    }

    #[test]
    fn file_hasher_for_protocol_27_falls_back_to_md4_with_seed() {
        // At negotiated < 30 the per-file digest stays on the legacy
        // CSUM_MD4_OLD path: MD4(seed_le || file_bytes). Verifies the
        // selector function picks MD4 below the proto-30 boundary.
        let seed = 0x1234_5678_i32;
        let mut h = FileHasher::for_protocol(seed, 27);
        h.update(b"payload");
        let got = h.finish();

        use md4::{Digest, Md4};
        let mut want = Md4::new();
        want.update(seed.to_le_bytes());
        want.update(b"payload");
        let want_d = want.finalize();
        assert_eq!(&got[..], want_d.as_slice());
    }

    #[test]
    fn file_hasher_md5_plain_ignores_seed() {
        // Two MD5 hashers with different seed inputs but the same
        // file bytes must produce identical digests — the MD5 path
        // does not absorb the seed. Regression guard against any
        // accidental seed prefix sneaking in.
        let mut a = FileHasher::for_protocol(0_i32, 31);
        a.update(b"data");
        let da = a.finish();

        let mut b = FileHasher::for_protocol(0xffff_ffff_u32 as i32, 31);
        b.update(b"data");
        let db = b.finish();

        assert_eq!(da, db, "MD5 path must ignore the seed");
    }

    #[test]
    fn hash_split_round_trips_with_combine() {
        // hash_split(buf) followed by combine_s1_s2 must reproduce
        // hash_fast(buf) byte-for-byte.
        for sample in [
            &b""[..],
            &b"a"[..],
            &b"ab"[..],
            &b"hello"[..],
            &[0xff_u8; 16][..],
            &[0_u8, 1, 2, 3, 4, 5, 6, 7, 8, 9][..],
        ] {
            let direct = super::hash_fast(sample);
            let (s1, s2) = super::hash_split(sample);
            let combined = super::combine_s1_s2(s1, s2);
            assert_eq!(direct, combined, "split/combine drift on {sample:?}");
        }
    }

    #[test]
    fn hash_roll_matches_hash_fast_after_one_step() {
        // Slide the window by one byte across a contiguous buffer and
        // confirm `hash_roll` produces the same packed hash that
        // recomputing `hash_fast` over the new window does. Uses a
        // 6-byte buffer with a 4-byte window so we exercise three
        // forward steps.
        let buf: &[u8] = b"abcdef";
        let block_len = 4_u32;
        let (mut s1, mut s2) = super::hash_split(&buf[0..4]);
        for offset in 1..3_usize {
            // Window slides: drop buf[offset-1], add buf[offset+block_len-1].
            let out_byte = buf[offset - 1];
            let in_byte = buf[offset + (block_len as usize) - 1];
            let (n1, n2) = super::hash_roll(s1, s2, out_byte, Some(in_byte), block_len);
            s1 = n1;
            s2 = n2;
            let combined = super::combine_s1_s2(s1, s2);
            let expected = super::hash_fast(&buf[offset..offset + block_len as usize]);
            assert_eq!(
                combined,
                expected,
                "rolling drift at offset {offset}: window={:?}",
                &buf[offset..offset + block_len as usize]
            );
        }
    }

    #[test]
    fn hash_roll_no_in_byte_only_removes() {
        // At end-of-buffer the caller passes in_byte=None: only the
        // outgoing byte's contribution is subtracted.
        let buf: &[u8] = b"abcd";
        let (s1, s2) = super::hash_split(buf);
        // Drop 'a' with no replacement.
        let (s1n, s2n) = super::hash_roll(s1, s2, b'a', None, 4);
        // Verify against manual subtraction: we know hash_fast ran on
        // the full buffer, so removing 'a' should leave (s1 -= 'a'),
        // (s2 -= 4 * 'a') with no add-in.
        let a_widened = super::sign_extend_byte(b'a');
        assert_eq!(s1n, s1.wrapping_sub(a_widened));
        assert_eq!(s2n, s2.wrapping_sub(4_u32.wrapping_mul(a_widened)));
    }

    #[test]
    fn hash_roll_handles_high_bit_bytes_with_sign_extension() {
        // Same sign-extension contract as hash_fast — a high-bit byte
        // (0xff) widens to 0xffff_ffff. Confirm that hash_roll respects
        // the same widening rule.
        let buf: &[u8] = &[0xff_u8, 0x00, 0x10, 0x20];
        let (s1, s2) = super::hash_split(buf);
        // Slide one byte: drop 0xff, add 0x30.
        let (s1n, s2n) = super::hash_roll(s1, s2, 0xff, Some(0x30), 4);
        let buf2: &[u8] = &[0x00_u8, 0x10, 0x20, 0x30];
        let expected = super::hash_fast(buf2);
        let combined = super::combine_s1_s2(s1n, s2n);
        assert_eq!(combined, expected);
    }

    #[test]
    fn file_hasher_for_protocol_30_uses_md5() {
        // Boundary check: the proto-30 boundary is INCLUSIVE for MD5
        // (rsync 3.2.7 `parse_csum_name` line 119: `if (protocol_version
        // >= 30)`). 30 itself is the MD5 path.
        let mut h = FileHasher::for_protocol(0_i32, 30);
        h.update(b"x");
        let got = h.finish();
        use md5::{Digest as Md5Digest, Md5};
        let mut want = Md5::new();
        Md5Digest::update(&mut want, b"x");
        assert_eq!(&got[..], want.finalize().as_slice());
    }
}
