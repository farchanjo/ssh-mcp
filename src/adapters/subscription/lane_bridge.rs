//! Lane-fanout bridge for the legacy broadcast pipeline.
//!
//! Wakes up `sub_open`-created lanes from the URI broadcast path
//! so stdio/HTTP transports get `notifications/resources/updated`
//! push delivery without a dedicated channel-mux drain task.
//! Composition root constructs one [`LaneFanoutBridge`] holding the
//! shared [`SubscriberLaneAdapter`] and the production
//! [`NotifierPort`], then installs it on the [`MemoryRegistry`] via
//! `install_lane_bridge`. Producer-side [`MemoryRegistry::broadcast`]
//! calls the bridge before the legacy peer fanout, fanning each push
//! out to every lane bound to the URI and incrementing per-lane
//! atomics.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tracing::debug;

use crate::adapters::subscription::subscriber_lane::SubscriberLaneAdapter;
use crate::ports::id_generator::IdGeneratorPort;
use crate::ports::notifier::{LaneNotifierBridge, NotifierPort};

/// Concrete [`LaneNotifierBridge`] implementation.
///
/// Generic over the id generator (matching [`SubscriberLaneAdapter`])
/// and the notifier adapter so the production wiring stays free of
/// `Box<dyn Trait>` for the hot path.
pub struct LaneFanoutBridge<I, N>
where
    I: IdGeneratorPort,
    N: NotifierPort + Send + Sync + 'static,
{
    lanes: Arc<SubscriberLaneAdapter<I>>,
    notifier: Arc<N>,
}

impl<I, N> fmt::Debug for LaneFanoutBridge<I, N>
where
    I: IdGeneratorPort,
    N: NotifierPort + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LaneFanoutBridge").finish_non_exhaustive()
    }
}

impl<I, N> LaneFanoutBridge<I, N>
where
    I: IdGeneratorPort,
    N: NotifierPort + Send + Sync + 'static,
{
    /// Construct a bridge from the shared lane adapter + notifier.
    #[must_use]
    pub fn new(lanes: Arc<SubscriberLaneAdapter<I>>, notifier: Arc<N>) -> Arc<Self> {
        Arc::new(Self { lanes, notifier })
    }
}

impl<I, N> LaneNotifierBridge for LaneFanoutBridge<I, N>
where
    I: IdGeneratorPort + Send + Sync + 'static,
    N: NotifierPort + Send + Sync + 'static,
{
    fn notify_lanes<'a>(
        &'a self,
        uri: &'a str,
        bytes_added: usize,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            let lanes = self.lanes.lanes_for_uri_public(uri);
            for lane in lanes {
                let Some(peer) = lane.peer().map(Arc::clone) else {
                    continue;
                };
                let peer_id = peer.id();
                if let Err(err) = self.notifier.notify_resource_updated(peer, uri).await {
                    debug!("lane peer notify failed for peer {peer_id}: {err}");
                    continue;
                }
                lane.record_notify(bytes_added);
            }
        })
    }
}
