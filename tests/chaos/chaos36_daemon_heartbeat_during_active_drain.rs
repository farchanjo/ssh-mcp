//! Chaos 36 — heartbeat during active high-rate event drain.
//!
//! The heartbeat task uses `try_send` so it never blocks the dispatcher
//! mpsc when the consumer is drained slowly. This run interleaves a
//! tight heartbeat (10 ms) with a high-rate event push and verifies
//! both flows interleave without deadlock and the consumer sees both
//! event types.

use std::time::Duration;

use ssh_mcp::composition::embed::wire_embed_transport;
use ssh_mcp::embed::event_mux::spawn_heartbeat;
use ssh_mcp::embed::formatter::Event;
use tokio_util::sync::CancellationToken;

const MUX_BUFFER: usize = 64;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chaos36_heartbeat_interleaves_with_high_rate_events() {
    let transport = wire_embed_transport(MUX_BUFFER).await.unwrap();
    let tx = transport.tx.clone();
    let mut rx = transport.rx;
    let shutdown = transport.handle.shutdown_token().clone();

    // Heartbeat at 10 ms.
    let hb_token = CancellationToken::new();
    let _hb = spawn_heartbeat(tx.clone(), Duration::from_millis(10), hb_token.clone());

    // Event pusher: 256 Warn events as fast as possible.
    let pusher = tokio::spawn({
        let tx = tx.clone();
        async move {
            let mut sent = 0_u32;
            for i in 0_u32..256 {
                let evt = Event::Warn {
                    code: format!("PUSH_{i}"),
                    resource: None,
                    msg: "push".to_string(),
                };
                if tx.send(evt).await.is_err() {
                    break;
                }
                sent += 1;
                if i.is_multiple_of(16) {
                    tokio::task::yield_now().await;
                }
            }
            sent
        }
    });

    // Consumer: drain for 500 ms, count Heartbeat + Warn separately.
    let consumer = tokio::spawn(async move {
        let mut warns = 0_u32;
        let mut heartbeats = 0_u32;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        loop {
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(Event::Warn { .. })) => warns += 1,
                Ok(Some(Event::Heartbeat { .. })) => heartbeats += 1,
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }
        (warns, heartbeats)
    });

    let sent = pusher.await.unwrap();
    let (warns, heartbeats) = consumer.await.unwrap();

    assert!(sent > 0, "pusher made no progress");
    assert!(warns > 0, "no warns delivered: heartbeats={heartbeats}");
    assert!(
        heartbeats >= 1,
        "no heartbeats delivered — task starved by event push: warns={warns}",
    );

    hb_token.cancel();
    shutdown.cancel();
    transport.handle.shutdown().await;
}
