//! Chaos 35 — embed transport: outbound mpsc saturation does not
//! deadlock.
//!
//! With a tiny mux buffer (`SSH_MUX_BUFFER`-equivalent = 1) and a
//! consumer that drains slowly, the producer (heartbeat / dispatcher)
//! must continue to make progress without blocking the runtime.
//! Heartbeats use `try_send` and drop on overflow; the test verifies
//! the embed transport survives the saturation and shuts down
//! cleanly.

use std::time::Duration;

use ssh_mcp::composition::embed::wire_embed_transport;
use ssh_mcp::embed::formatter::Event;

const MUX_BUFFER: usize = 1;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chaos35_outbound_buffer_full_does_not_deadlock() {
    let transport = wire_embed_transport(MUX_BUFFER).await.unwrap();
    let tx = transport.tx.clone();
    let token = transport.handle.shutdown_token().clone();

    // Saturator: try_send 256 events as fast as possible; consumer is
    // paused so nearly every push fails — but try_send NEVER awaits.
    let saturator = tokio::spawn({
        let tx = tx.clone();
        async move {
            let mut accepted = 0_u32;
            for i in 0_u32..256 {
                let evt = Event::Warn {
                    code: format!("CHAOS_{i}"),
                    resource: None,
                    msg: "saturate".to_string(),
                };
                if tx.try_send(evt).is_ok() {
                    accepted += 1;
                }
                tokio::task::yield_now().await;
            }
            accepted
        }
    });

    // Slow consumer: pulls one event every 5 ms for a bounded window
    // so the saturator can race against forward progress.
    let mut rx = transport.rx;
    let consumer = tokio::spawn(async move {
        let mut received = 0_u32;
        for _ in 0_u32..16 {
            if let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await {
                received += 1;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        received
    });

    let accepted = saturator.await.unwrap();
    let received = consumer.await.unwrap();
    // Some events were accepted (the consumer drained at least the
    // initial buffer) — the run made forward progress.
    assert!(accepted > 0 || received > 0, "no progress at all");
    // Saturation could not have accepted more than buffer + drained.
    assert!(
        accepted <= u32::try_from(MUX_BUFFER).unwrap_or(u32::MAX) + received + 1,
        "accepted={accepted} buffer={MUX_BUFFER} received={received}",
    );

    // Shutdown is clean (no deadlock).
    token.cancel();
    transport.handle.shutdown().await;
    drop(tx);
}
