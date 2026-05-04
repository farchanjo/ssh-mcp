//! Round-robin channel mux port (v5 Phase 2).
//!
//! Provides aggregate stats for the `ssh_daemon_stats` tool surface
//! (Phase 4). The mux itself runs as a background task driven by the
//! adapter; the port surface is intentionally minimal so the use
//! cases stay decoupled from the round-robin driver.
//!
//! See [ADR 0004](../docs/adr/0004-channel-mux-fairness.md).

/// Aggregate counters across every active lane.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AggregateStats {
    /// Number of active lanes (sum across resources).
    pub active_lanes: usize,
    /// Sum of `events_sent` across every lane.
    pub events_sent: u64,
    /// Sum of `bytes_sent` across every lane.
    pub bytes_sent: u64,
    /// Sum of `lagged_drops` across every lane.
    pub lagged_drops: u64,
    /// Sum of `lagged_recoveries` across every lane.
    pub lagged_recoveries: u64,
    /// Maximum observed `queue_high_watermark` across every lane.
    pub max_queue_high_watermark: usize,
}

/// Sync, dyn-safe slice of the [`crate::adapters::subscription::channel_mux::ChannelMux`].
pub trait ChannelMuxPort: Send + Sync + 'static {
    /// Number of currently active lanes.
    fn active_lane_count(&self) -> usize;

    /// Aggregate stats snapshot.
    fn aggregate_stats(&self) -> AggregateStats;
}

#[cfg(test)]
mod tests {
    use super::{AggregateStats, ChannelMuxPort};

    fn _assert_dyn_safe(_p: &dyn ChannelMuxPort) {}

    #[test]
    fn aggregate_stats_default_is_all_zero() {
        let s = AggregateStats::default();
        assert_eq!(s.active_lanes, 0);
        assert_eq!(s.events_sent, 0);
        assert_eq!(s.bytes_sent, 0);
        assert_eq!(s.lagged_drops, 0);
        assert_eq!(s.lagged_recoveries, 0);
        assert_eq!(s.max_queue_high_watermark, 0);
    }

    #[test]
    fn aggregate_stats_eq_and_hash_for_dashmap_keys() {
        let s = AggregateStats {
            active_lanes: 3,
            events_sent: 100,
            bytes_sent: 4_096,
            lagged_drops: 1,
            lagged_recoveries: 0,
            max_queue_high_watermark: 32,
        };
        let twin = s;
        assert_eq!(s, twin);
        let mut set = std::collections::HashSet::new();
        set.insert(s);
        set.insert(twin);
        assert_eq!(set.len(), 1);
    }
}
