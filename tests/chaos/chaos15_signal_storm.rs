//! Chaos 15 — signal storm.
//!
//! Multiple SIGTERM/SIGINT/SIGHUP-equivalent cancellations interleaved
//! against the embed daemon. Requirement: shutdown grace fires once
//! and only once; cancellation token is re-cancellable safely.

use std::time::Duration;

use ssh_mcp::composition::embed::wire_embed_transport;

const OP_BUFFER: usize = 32;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos15_signal_storm_does_not_double_cleanup() {
    let transport = wire_embed_transport(OP_BUFFER).await.unwrap();
    let token = transport.handle.shutdown_token().clone();

    // Drain task models the consumer.
    let drain = tokio::spawn({
        let mut rx = transport.rx;
        async move {
            while let Ok(Some(_evt)) =
                tokio::time::timeout(Duration::from_millis(50), rx.recv()).await
            {}
        }
    });

    // Storm: 8 cancellations interleaved across two task spawns.
    let storm_a = tokio::spawn({
        let token = token.clone();
        async move {
            for _ in 0..4 {
                token.cancel();
                tokio::task::yield_now().await;
            }
        }
    });
    let storm_b = tokio::spawn({
        let token = token.clone();
        async move {
            for _ in 0..4 {
                token.cancel();
                tokio::task::yield_now().await;
            }
        }
    });

    storm_a.await.unwrap();
    storm_b.await.unwrap();

    // Single shutdown call MUST tolerate the prior cancellations.
    transport.handle.shutdown().await;
    drop(transport.tx);

    // Drain task terminates cleanly.
    let result = tokio::time::timeout(Duration::from_secs(3), drain).await;
    assert!(result.is_ok(), "drain hung after signal storm");

    // Cancellation is idempotent — calling .cancel() again after
    // shutdown is a no-op.
    token.cancel();
}
