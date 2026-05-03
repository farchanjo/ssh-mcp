//! [`crate::ports::notifier::NotifierPort`] adapters.
//!
//! - [`rmcp_peer::RmcpPeerHandle`] — concrete [`PeerHandle`] wrapping
//!   [`rmcp::Peer<RoleServer>`]. Registers itself in a shared lookup table
//!   on construction and unregisters on `Drop` so the matching
//!   [`rmcp_adapter::RmcpNotifier`] can recover the underlying rmcp peer
//!   from a type-erased [`Arc<dyn PeerHandle>`] without breaking the
//!   port's dyn-safety constraints.
//! - [`rmcp_adapter::RmcpNotifier`] — production adapter that fans out
//!   `notifications/resources/updated` over the rmcp transport.
//!
//! Both adapters share an [`rmcp_peer::PeerTable`] so the notifier can
//! resolve [`crate::domain::ids::PeerId`] -> live rmcp peer in O(1) without
//! requiring [`crate::ports::notifier::PeerHandle`] to extend
//! [`std::any::Any`].
//!
//! [`PeerHandle`]: crate::ports::notifier::PeerHandle
//! [`Arc<dyn PeerHandle>`]: std::sync::Arc

pub mod rmcp_adapter;
pub mod rmcp_peer;
