//! [`SubscriberRegistryPort`] / [`SubscriberRegistryAsync`] adapters.
//!
//! - [`memory_registry::MemoryRegistry`] — in-process, lock-free
//!   registry built on [`dashmap::DashMap`]. Direct port of the v3
//!   `SubscriptionRegistry` ([`crate::mcp::subscription`]) with the
//!   debouncer / keepalive / sequence / per-peer cursor semantics
//!   preserved. The v3 module stays in place until H17 retires it.
//!
//! [`SubscriberRegistryPort`]: crate::ports::subscriber_registry::SubscriberRegistryPort
//! [`SubscriberRegistryAsync`]: crate::ports::subscriber_registry::SubscriberRegistryAsync

pub mod memory_registry;
