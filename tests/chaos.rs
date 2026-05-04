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

#[path = "chaos/chaos16_sub_pause_resume_race_vs_producer.rs"]
mod chaos16_sub_pause_resume_race_vs_producer;

#[path = "chaos/chaos17_filter_regex_hot_reload_during_emission.rs"]
mod chaos17_filter_regex_hot_reload_during_emission;

#[path = "chaos/chaos18_replay_during_concurrent_producer.rs"]
mod chaos18_replay_during_concurrent_producer;

#[path = "chaos/chaos19_sub_list_during_burst_churn.rs"]
mod chaos19_sub_list_during_burst_churn;

#[path = "chaos/chaos20_sub_stats_read_during_active_writes.rs"]
mod chaos20_sub_stats_read_during_active_writes;

#[path = "chaos/chaos21_lag_policy_block_slow_timeout_fallback.rs"]
mod chaos21_lag_policy_block_slow_timeout_fallback;

#[path = "chaos/chaos22_lag_policy_drop_newest_verify.rs"]
mod chaos22_lag_policy_drop_newest_verify;

#[path = "chaos/chaos23_channel_mux_fairness_n_lanes.rs"]
mod chaos23_channel_mux_fairness_n_lanes;

#[path = "chaos/chaos24_release_when_no_subs_grace_vs_resubscribe.rs"]
mod chaos24_release_when_no_subs_grace_vs_resubscribe;

#[path = "chaos/chaos25_cascade_refcount_many_simultaneous_close.rs"]
mod chaos25_cascade_refcount_many_simultaneous_close;

#[path = "chaos/chaos26_session_refcount_underflow_attempt.rs"]
mod chaos26_session_refcount_underflow_attempt;

#[path = "chaos/chaos27_grace_fire_vs_explicit_close_race.rs"]
mod chaos27_grace_fire_vs_explicit_close_race;

#[path = "chaos/chaos28_filter_regex_compile_failure_runtime.rs"]
mod chaos28_filter_regex_compile_failure_runtime;

#[path = "chaos/chaos29_ring_buffer_overflow_replay_beyond_window.rs"]
mod chaos29_ring_buffer_overflow_replay_beyond_window;

#[path = "chaos/chaos30_subid_uuidv7_ordering_concurrent_subscribe.rs"]
mod chaos30_subid_uuidv7_ordering_concurrent_subscribe;

#[path = "chaos/chaos31_sub_leak_risk_warn_vs_sub_arrives_in_window.rs"]
mod chaos31_sub_leak_risk_warn_vs_sub_arrives_in_window;

#[path = "chaos/chaos32_leak_watcher_kill_phase_active.rs"]
mod chaos32_leak_watcher_kill_phase_active;

#[path = "chaos/chaos33_leak_warn_bridge_progress_token_disabled.rs"]
mod chaos33_leak_warn_bridge_progress_token_disabled;

#[path = "chaos/chaos34_ndjson_line_size_overflow.rs"]
mod chaos34_ndjson_line_size_overflow;

#[path = "chaos/chaos35_embed_transport_buffer_full.rs"]
mod chaos35_embed_transport_buffer_full;

#[path = "chaos/chaos36_daemon_heartbeat_during_active_drain.rs"]
mod chaos36_daemon_heartbeat_during_active_drain;

#[path = "chaos/chaos37_v4_legacy_subscribe_with_v5_features.rs"]
mod chaos37_v4_legacy_subscribe_with_v5_features;
