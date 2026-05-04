//! [`SubscriberRegistryPort`] / [`SubscriberRegistryAsync`] adapters.
//!
//! - [`memory_registry::MemoryRegistry`] — in-process, lock-free
//!   registry built on [`dashmap::DashMap`]. Direct port of the v3
//!   `SubscriptionRegistry` (now [`legacy::SubscriptionRegistry`]) with
//!   the debouncer / keepalive / sequence / per-peer cursor semantics
//!   preserved.
//! - [`legacy`] — the legacy [`legacy::SubscriptionRegistry`] singleton
//!   plus [`legacy::spawn_peer_gc`]. Relocated here from the former
//!   v3 subscription module in H17.6 P3 because the SSH/SFTP runtime
//!   adapters still write event sequence numbers and pokes through the
//!   global. The hexagonal [`memory_registry::MemoryRegistry`] is the
//!   forward-looking replacement; legacy lives on under `legacy::*`
//!   until the runtime adapters are migrated to the port surface.
//!
//! [`SubscriberRegistryPort`]: crate::ports::subscriber_registry::SubscriberRegistryPort
//! [`SubscriberRegistryAsync`]: crate::ports::subscriber_registry::SubscriberRegistryAsync

pub mod channel_mux;
pub mod filter;
pub mod lane_bridge;
pub mod legacy;
pub mod memory_registry;
pub mod replay;
pub mod subscriber_lane;
