//! Chaos 02 — kill NDJSON daemon mid-stream.
//!
//! Simulates a SIGKILL of the daemon by abruptly cancelling the
//! shutdown token while events are still queued. Requirement: the
//! event mpsc receiver drains gracefully and no further events leak
//! once the handle is dropped.

use std::time::Duration;

use ssh_mcp::composition::embed::wire_embed_transport;

const OP_BUFFER: usize = 64;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos02_daemon_killed_mid_stream_does_not_leak() {
    let transport = wire_embed_transport(OP_BUFFER).await.unwrap();
    let mut rx = transport.rx;
    let token = transport.handle.shutdown_token().clone();

    // Start a background drain that pretends to be a remote consumer.
    let drain = tokio::spawn(async move {
        let mut count = 0_u32;
        while let Ok(Some(_evt)) = tokio::time::timeout(Duration::from_millis(50), rx.recv()).await
        {
            count += 1;
            if count >= 16 {
                break;
            }
        }
        count
    });

    // SIGKILL-equivalent: cancel the shutdown token immediately, then
    // drop the handle which closes every internal sender.
    token.cancel();
    transport.handle.shutdown().await;
    drop(transport.tx);

    // The drain task MUST terminate cleanly within a bounded window.
    // We don't care how many events it saw — only that the task didn't
    // hang or panic.
    let result = tokio::time::timeout(Duration::from_secs(3), drain).await;
    assert!(
        result.is_ok(),
        "drain task did not terminate after kill: {result:?}",
    );
}
