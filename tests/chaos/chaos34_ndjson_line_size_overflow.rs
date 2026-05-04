//! Chaos 34 — NDJSON line size overflow.
//!
//! A single line longer than `SSH_NDJSON_LINE_MAX` MUST surface an
//! `Invalid` outcome (the dispatcher emits `INVALID_OP`). Subsequent
//! short lines still parse cleanly — the reader does NOT poison.

use std::io::Cursor;

use ssh_mcp::embed::parser::{NdjsonReader, ParseError, ParseOutcome};
use tokio::io::BufReader;

const LINE_MAX: usize = 256;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chaos34_ndjson_oversized_line_emits_invalid_then_recovers() {
    // Build a payload with three lines:
    // 1. small valid op (parses)
    // 2. line exceeding LINE_MAX (rejected)
    // 3. small valid op (parses again — reader is not poisoned)
    let mut payload = String::new();
    payload.push_str("{\"op\":\"shutdown\",\"id\":\"a\"}\n");
    let huge_filler = "x".repeat(LINE_MAX * 2);
    payload.push_str(&format!(
        "{{\"op\":\"shutdown\",\"id\":\"{huge_filler}\"}}\n"
    ));
    payload.push_str("{\"op\":\"shutdown\",\"id\":\"c\"}\n");

    let mut reader =
        NdjsonReader::with_line_max(BufReader::new(Cursor::new(payload.into_bytes())), LINE_MAX);

    // Line 1: valid.
    match reader.next().await {
        ParseOutcome::Op(_) => {}
        other => panic!("expected Op for line 1, got {other:?}"),
    }

    // Line 2: rejected because it exceeds LINE_MAX.
    match reader.next().await {
        ParseOutcome::Invalid(ParseError::LineTooLong(n)) => {
            assert!(n > LINE_MAX, "reported size below cap: {n}");
        }
        other => panic!("expected LineTooLong for line 2, got {other:?}"),
    }

    // Line 3: still parses — reader is not poisoned.
    match reader.next().await {
        ParseOutcome::Op(_) => {}
        other => panic!("expected Op for line 3, got {other:?}"),
    }

    // Reader hits EOF cleanly.
    matches!(reader.next().await, ParseOutcome::Eof);
}
