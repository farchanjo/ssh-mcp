//! v5 Phase 5 — chaos test entry point.
//!
//! Cargo only auto-discovers `tests/<name>.rs` as integration test
//! targets. Files in `tests/chaos/` are not picked up directly, so
//! this entry point pulls each scenario in as a sibling module via
//! `#[path = "..."]`. Each scenario lives in its own file so the
//! suite stays diffable; this file is the single binary cargo
//! compiles and links.
//!
//! Every scenario exercises in-memory adapters only — there is no
//! live SSH server in any chaos test. The `test-fixtures` Cargo
//! feature is required (it enables the deterministic in-memory
//! adapters such as `FakeClock` + `DeterministicIdGenerator`).
//!
//! Run with:
//!
//! ```text
//! cargo test --features test-fixtures --test chaos
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_panics_doc,
    clippy::missing_errors_doc,
    clippy::cast_possible_truncation,
    clippy::cast_lossless,
    clippy::cast_sign_loss,
    clippy::module_name_repetitions,
    clippy::default_numeric_fallback,
    clippy::implicit_hasher,
    reason = "chaos integration tests use unwrap/panic for brevity and exercise deliberate failure paths"
)]

#[path = "chaos/chaos01_kill_subscriber_mid_stream.rs"]
mod chaos01_kill_subscriber_mid_stream;

#[path = "chaos/chaos02_kill_daemon_mid_stream.rs"]
mod chaos02_kill_daemon_mid_stream;

#[path = "chaos/chaos03_network_partition.rs"]
mod chaos03_network_partition;

#[path = "chaos/chaos04_slow_consumer_overflow.rs"]
mod chaos04_slow_consumer_overflow;

#[path = "chaos/chaos05_burst_subscribe_unsubscribe.rs"]
mod chaos05_burst_subscribe_unsubscribe;

#[path = "chaos/chaos06_concurrent_disconnect_subscribe.rs"]
mod chaos06_concurrent_disconnect_subscribe;

#[path = "chaos/chaos07_clock_skew.rs"]
mod chaos07_clock_skew;

#[path = "chaos/chaos08_disk_full_during_upload.rs"]
mod chaos08_disk_full_during_upload;

#[path = "chaos/chaos09_kill_remote_process_mid_command.rs"]
mod chaos09_kill_remote_process_mid_command;

#[path = "chaos/chaos10_too_many_subs_per_uri.rs"]
mod chaos10_too_many_subs_per_uri;

#[path = "chaos/chaos11_mpsc_full_panic_safety.rs"]
mod chaos11_mpsc_full_panic_safety;

#[path = "chaos/chaos12_session_inactivity_during_active_sub.rs"]
mod chaos12_session_inactivity_during_active_sub;

#[path = "chaos/chaos13_idempotency_replay.rs"]
mod chaos13_idempotency_replay;

#[path = "chaos/chaos14_uuidv7_collision_simulation.rs"]
mod chaos14_uuidv7_collision_simulation;

#[path = "chaos/chaos15_signal_storm.rs"]
mod chaos15_signal_storm;
