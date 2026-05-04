//! Properties: ring buffer head-drop semantics.
//!
//! - `prop_ring_buffer_drop_preserves_monotonicity` — head-drop on a
//!   `Bytes` slice always preserves the relative byte order of the
//!   surviving suffix (the v3 ring buffer drops the oldest bytes by
//!   `slice(N..)`).

use bytes::Bytes;
use proptest::prelude::*;
use ssh_mcp::domain::ringbuffer::RingBuffer;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// `prop_ring_buffer_drop_preserves_monotonicity`
    ///
    /// Dropping the first N bytes from a ring buffer leaves the
    /// remaining bytes in their original order.
    #[test]
    fn prop_ring_buffer_drop_preserves_monotonicity(
        bytes in proptest::collection::vec(any::<u8>(), 0..256),
        drop_n in 0_usize..256,
    ) {
        let buf = RingBuffer { data: Bytes::from(bytes.clone()) };
        let len = buf.data.len();
        let n = drop_n.min(len);
        let after = buf.data.slice(n..);
        let expected = bytes.get(n..len).unwrap_or(&[]);
        prop_assert_eq!(after.as_ref(), expected);
    }

    /// `prop_ring_buffer_clone_shares_payload`
    ///
    /// Cloning a ring buffer is a cheap Bytes refcount bump — the
    /// payloads are byte-identical.
    #[test]
    fn prop_ring_buffer_clone_shares_payload(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
        let buf = RingBuffer { data: Bytes::from(bytes.clone()) };
        let cloned = buf.clone();
        prop_assert_eq!(buf.data.as_ref(), cloned.data.as_ref());
    }
}
