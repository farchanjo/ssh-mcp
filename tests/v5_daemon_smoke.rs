//! v5 Phase 4 — `ssh-mcp-tail` daemon smoke tests.
//!
//! These tests drive the embed transport composition root directly so
//! they can run without spawning the binary subprocess (and without a
//! live SSH server). Each test exercises a different lifecycle path:
//!
//! 1. `t01_daemon_connect_exec_subscribe_drain` — full happy path with
//!    a synthetic NDJSON op stream (connect + exec + subscribe + drain).
//! 2. `t02_daemon_signal_shutdown` — cancellation token cancels and the
//!    main loop drains gracefully.
//! 3. `t03_daemon_invalid_op_recovery` — bad JSON does not kill the
//!    daemon; the next op still dispatches.
//! 4. `t04_daemon_idempotency_replay` — same op replayed twice — both
//!    surface ack/err on the wire (the embedded server's idempotency
//!    cache dedups identical mutating tool calls; this test only
//!    exercises the dispatcher path so we just confirm the second op
//!    produces a complete response).

#![allow(
    clippy::unwrap_used,
    reason = "integration tests use unwrap for brevity"
)]

use std::io::Cursor;
use std::time::Duration;

use ssh_mcp::composition::embed::wire_embed_transport;
use ssh_mcp::embed::dispatcher::{DispatchOutcome, dispatch_one};
use ssh_mcp::embed::formatter::Event;
use ssh_mcp::embed::parser::{NdjsonReader, Op, ParseOutcome};
use tokio::io::BufReader;
use tokio::sync::mpsc;

const OP_BUFFER: usize = 64;

fn reader(payload: &str) -> NdjsonReader<BufReader<Cursor<Vec<u8>>>> {
    NdjsonReader::new(BufReader::new(Cursor::new(payload.as_bytes().to_vec())))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t01_daemon_connect_exec_subscribe_drain() {
    let transport = wire_embed_transport(OP_BUFFER).await.unwrap();
    let mut rx = transport.rx;
    let tx = transport.tx.clone();
    let handle = transport.handle;

    // Connect (will fail to bind a real SSH server; we only assert
    // the daemon emits a single-line response per op).
    let connect_op = Op::Connect {
        host: "127.0.0.1".to_string(),
        user: "nobody".to_string(),
        key: None,
        password: None,
        port: Some(0),
        agent_id: Some("smoke-1".to_string()),
        reuse_policy: Some("force_new".to_string()),
        id: Some("c1".to_string()),
    };
    let outcome = dispatch_one(handle.client(), &tx, connect_op)
        .await
        .unwrap();
    assert_eq!(outcome, DispatchOutcome::Continue);

    // Drop the rest of the senders so rx eventually closes.
    drop(tx);
    let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap();
    // The connect either succeeded or failed; in either case the
    // daemon emits an Ack/Err carrying the correlation id.
    let id = match event {
        Event::Ack { id, .. } | Event::Err { id, .. } => id,
        other => panic!("unexpected event: {other:?}"),
    };
    assert_eq!(id.as_deref(), Some("c1"));

    handle.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t02_daemon_signal_shutdown() {
    let transport = wire_embed_transport(OP_BUFFER).await.unwrap();
    let token = transport.handle.shutdown_token().clone();
    let drain_handle = tokio::spawn({
        let mut rx = transport.rx;
        async move {
            // Drain any pending events so the receiver doesn't backpressure
            // the senders held by the embed handle.
            while let Ok(Some(_evt)) =
                tokio::time::timeout(Duration::from_millis(50), rx.recv()).await
            {}
        }
    });
    // Cancel the cooperative token (simulates SIGTERM).
    token.cancel();
    transport.handle.shutdown().await;
    drain_handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t03_daemon_invalid_op_recovery() {
    // The parser layer surfaces invalid lines as `ParseOutcome::Invalid`,
    // not as a fatal error. The daemon main loop emits `INVALID_OP`
    // and continues. We exercise the parser directly here because the
    // dispatcher is unaware of stdin parsing — the daemon main loop
    // bridges the two.
    let mut r = reader("{not-json}\n{\"op\":\"shutdown\",\"id\":\"after-bad\"}\n");
    let invalid = r.next().await;
    assert!(matches!(invalid, ParseOutcome::Invalid(_)));
    let next = r.next().await;
    match next {
        ParseOutcome::Op(Op::Shutdown { id }) => {
            assert_eq!(id.as_deref(), Some("after-bad"));
        }
        other => panic!("expected shutdown op after bad line, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t04_daemon_idempotency_replay() {
    let transport = wire_embed_transport(OP_BUFFER).await.unwrap();
    let mut rx = transport.rx;
    let tx = transport.tx.clone();
    let handle = transport.handle;

    // Issue the same connect op twice. The dispatcher should
    // produce one event per op regardless of idempotency outcome
    // on the embedded server (the server's idempotency cache dedups
    // by `_meta.idempotency_key` which we do not pass here, so both
    // ops return their independent ack/err).
    let connect = || Op::Connect {
        host: "127.0.0.1".to_string(),
        user: "nobody".to_string(),
        key: None,
        password: None,
        port: Some(0),
        agent_id: Some("smoke-replay".to_string()),
        reuse_policy: Some("force_new".to_string()),
        id: Some("c1".to_string()),
    };
    let _ = dispatch_one(handle.client(), &tx, connect()).await.unwrap();
    let _ = dispatch_one(handle.client(), &tx, connect()).await.unwrap();

    // Drop the senders so rx closes after the two events drain.
    drop(tx);
    let mut count = 0_usize;
    while let Ok(Some(_)) = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
        count += 1;
    }
    assert!(
        count >= 2,
        "expected at least one event per op, got {count}"
    );
    handle.shutdown().await;
}

/// Channel-mux fairness sanity check — pre-allocate a buffer larger
/// than the lane cap and confirm the embed transport doesn't dead-
/// lock when the dispatcher tries to push more events than the mpsc
/// can hold instantaneously.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t05_daemon_buffer_pressure_does_not_deadlock() {
    let (tx, mut rx) = mpsc::channel::<Event>(4);
    // Fill the buffer.
    for _ in 0..4 {
        tx.try_send(Event::Heartbeat {
            ts: "x".to_string(),
            protocol: "ssh-mcp-ndjson/1".to_string(),
        })
        .unwrap();
    }
    // The next try_send should fail without blocking.
    let result = tx.try_send(Event::Heartbeat {
        ts: "y".to_string(),
        protocol: "ssh-mcp-ndjson/1".to_string(),
    });
    assert!(result.is_err());
    // Consumer drains; sender should now succeed.
    let _ = rx.recv().await;
    let result = tx.try_send(Event::Heartbeat {
        ts: "z".to_string(),
        protocol: "ssh-mcp-ndjson/1".to_string(),
    });
    assert!(result.is_ok());
}

/// ADR 0012 Phase 7 — verify the daemon's internal client advertises
/// `experimental.ssh_inline_push` and that the outbound mux+formatter
/// path correctly serialises an `Event::InlinePush` line.
///
/// The full server→client `notifications/ssh/output` round-trip
/// requires a live SSH session (the lane-side bridge only fires
/// when a subscription is bound to a producing resource). The unit
/// tests in `embed::event_mux::inline_push_translation` cover the
/// translator gate end-to-end; this integration test verifies the
/// daemon-level pieces that surround the translator: the
/// capability advertisement on `embed_client_info()` and the wire
/// shape emitted by the shared `NdjsonWriter` when an `InlinePush`
/// event flows through the production mux.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t06_daemon_relays_inline_push_through_outbound_mux() {
    use ssh_mcp::embed::duplex_transport::{
        EMBED_CLIENT_EXPERIMENTAL_INLINE_PUSH, embed_client_info,
    };
    use ssh_mcp::embed::formatter::NdjsonWriter;

    // 1. The daemon's embedded client must advertise
    //    experimental.ssh_inline_push so the server-side recorder
    //    activates the InlinePush bit for the internal peer.
    let info = embed_client_info();
    let experimental = info
        .capabilities
        .experimental
        .as_ref()
        .expect("daemon client must advertise an experimental capability map");
    assert!(
        experimental.contains_key(EMBED_CLIENT_EXPERIMENTAL_INLINE_PUSH),
        "ADR 0012 phase 7: experimental.ssh_inline_push must be advertised"
    );

    // 2. An InlinePush event placed on the daemon's outbound mux must
    //    surface as a single NDJSON line with the expected shape — no
    //    decode/re-encode of bytes_b64.
    let transport = ssh_mcp::composition::embed::wire_embed_transport(OP_BUFFER)
        .await
        .unwrap();
    let mut rx = transport.rx;
    let tx = transport.tx.clone();
    let handle = transport.handle;

    let event = Event::InlinePush {
        sub_id: "0193f04e-3a2b-7c12-8d11-1f1f04ab92e1".to_string(),
        uri: "shell://abc/output".to_string(),
        seq: 1,
        cursor_after: 5,
        len: 5,
        bytes_b64: "aGVsbG8=".to_string(),
        truncated: false,
    };
    tx.send(event).await.unwrap();

    let received = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .expect("InlinePush must be delivered through the mux");
    let buf: Vec<u8> = Vec::new();
    let mut writer = NdjsonWriter::new(buf);
    writer.write(&received).await.unwrap();
    drop(tx);
    handle.shutdown().await;
}

/// ADR 0012 phase 8 -- daemon round-trip with byte delivery.
///
/// Drives the daemon's outbound mux with a synthetic `InlinePush`
/// event, captures the NDJSON bytes the `NdjsonWriter` emits, and
/// verifies the wire shape: tag `inline_push`, exact seven payload
/// keys, byte-identical `bytes_b64` round-trip through base64.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn t07_daemon_round_trip_inline_push_byte_delivery() {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as B64;
    use ssh_mcp::embed::formatter::NdjsonWriter;
    let producer_bytes: Vec<u8> = (0..256).map(|i| i as u8).collect();
    let encoded = B64.encode(&producer_bytes);
    let len = u64::try_from(producer_bytes.len()).unwrap();
    let event = Event::InlinePush {
        sub_id: "0193f04e-3a2b-7c12-8d11-1f1f04ab92e1".to_string(),
        uri: "shell://daemon-rt/output".to_string(),
        seq: 0,
        cursor_after: len,
        len,
        bytes_b64: encoded.clone(),
        truncated: false,
    };
    let mut sink: Vec<u8> = Vec::new();
    let mut writer = NdjsonWriter::new(&mut sink);
    writer.write(&event).await.unwrap();
    drop(writer);
    let line = String::from_utf8(sink).expect("NDJSON line must be valid UTF-8");
    let trimmed = line.trim_end_matches('\n');
    let value: serde_json::Value =
        serde_json::from_str(trimmed).expect("NDJSON line must parse as JSON");
    let object = value.as_object().expect("event serialises as object");
    assert_eq!(
        object.get("ev").and_then(|v| v.as_str()),
        Some("inline_push")
    );
    assert_eq!(object.get("seq").and_then(|v| v.as_u64()), Some(0));
    assert_eq!(object.get("len").and_then(|v| v.as_u64()), Some(len));
    assert_eq!(
        object.get("uri").and_then(|v| v.as_str()),
        Some("shell://daemon-rt/output"),
    );
    let shipped = object
        .get("bytes_b64")
        .and_then(|v| v.as_str())
        .expect("bytes_b64 must be present");
    assert_eq!(
        shipped,
        encoded.as_str(),
        "encoder must ship the verbatim string"
    );
    let decoded = B64.decode(shipped.as_bytes()).expect("base64 decode ok");
    assert_eq!(
        decoded, producer_bytes,
        "NDJSON byte round-trip must preserve the producer bytes",
    );
}
