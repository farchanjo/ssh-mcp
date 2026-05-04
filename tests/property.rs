//! v5 Phase 5 — property test entry point.
//!
//! Cargo only auto-discovers `tests/<name>.rs` as integration test
//! targets. Files in `tests/property/` are pulled in as sibling
//! modules below. Each property is a `proptest!` macro invocation
//! against the in-memory adapters (no live SSH server).
//!
//! Run with:
//!
//! ```text
//! cargo test --features test-fixtures --test property
//! ```
//!
//! The properties are grouped by domain (state machine, cursor,
//! filter, lag policy, ndjson roundtrip, cascade) and total 25 +
//! across the suite.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc,
    clippy::missing_errors_doc,
    clippy::cast_possible_truncation,
    clippy::cast_lossless,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::module_name_repetitions,
    clippy::default_numeric_fallback,
    clippy::implicit_hasher,
    clippy::needless_pass_by_value,
    clippy::wildcard_imports,
    reason = "property tests use unwrap and proptest macros that pull broad imports — strict suppressions apply only to the test target"
)]

#[path = "property/state_machine.rs"]
mod state_machine;

#[path = "property/cursor.rs"]
mod cursor;

#[path = "property/idempotency.rs"]
mod idempotency;

#[path = "property/ndjson.rs"]
mod ndjson;

#[path = "property/lag_policy.rs"]
mod lag_policy;

#[path = "property/filter.rs"]
mod filter;

#[path = "property/cascade.rs"]
mod cascade;

#[path = "property/replay.rs"]
mod replay;

#[path = "property/ringbuffer.rs"]
mod ringbuffer;

#[path = "property/subscribe_unsubscribe.rs"]
mod subscribe_unsubscribe;

#[path = "property/grace_timer.rs"]
mod grace_timer;

#[path = "property/lane_mpsc.rs"]
mod lane_mpsc;

#[path = "property/uuid_v7.rs"]
mod uuid_v7;
