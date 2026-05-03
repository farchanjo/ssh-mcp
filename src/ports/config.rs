//! Configuration accessor port.
//!
//! Mirrors the resolver functions in [`crate::mcp::config`] so use cases can
//! query tunables without touching environment variables directly. The port
//! is sync and dyn-safe; the production adapter (etapa H6) wraps the v3
//! resolvers, while tests inject a simple struct literal.

use std::time::Duration;

/// Read-only configuration port.
#[allow(
    clippy::too_many_lines,
    reason = "config aggregator surfaces every tunable in a single trait so adapters can implement it once"
)]
pub trait ConfigPort: Send + Sync + 'static {
    /// SSH connect timeout (`SSH_CONNECT_TIMEOUT`).
    fn connect_timeout(&self) -> Duration;

    /// Default per-command timeout (`SSH_COMMAND_TIMEOUT`).
    fn command_timeout(&self) -> Duration;

    /// Maximum SSH connect retry attempts (`SSH_MAX_RETRIES`).
    fn max_retries(&self) -> u32;

    /// Initial retry backoff delay (`SSH_RETRY_DELAY_MS`).
    fn retry_delay(&self) -> Duration;

    /// Inactive-session sweep interval (`SSH_INACTIVITY_TIMEOUT`).
    fn inactivity_timeout(&self) -> Duration;

    /// Whether SSH zlib compression is enabled by default (`SSH_COMPRESSION`).
    fn compression_enabled(&self) -> bool;

    /// TTL before unread command output is reclaimed (`SSH_COMMAND_CLEANUP_TTL`).
    fn command_cleanup_ttl(&self) -> Duration;

    /// TTL before terminated transfers are reclaimed (`SSH_TRANSFER_CLEANUP_TTL`).
    fn transfer_cleanup_ttl(&self) -> Duration;

    /// Default shell auto-close TTL (`SSH_SHELL_INACTIVITY_TTL`).
    fn shell_inactivity_ttl(&self) -> Duration;

    /// Default rolling shell-output buffer size (`SSH_SHELL_MAX_BUFFER_SIZE`).
    fn shell_max_buffer_size(&self) -> u64;

    /// Default per-command output buffer size (`SSH_COMMAND_MAX_BUFFER_SIZE`).
    fn command_max_buffer_size(&self) -> u64;

    /// Default `max_output_bytes` for output-returning tools.
    fn output_default_bytes(&self) -> usize;

    /// Hard cap on `max_output_bytes`.
    fn output_max_bytes_cap(&self) -> usize;

    /// Default `max_items` for list tools.
    fn list_max_items_default(&self) -> usize;

    /// Hard cap on `max_items`.
    fn list_max_items_cap(&self) -> usize;

    /// Broadcast channel capacity for command-output fan-out.
    fn command_broadcast_cap(&self) -> usize;

    /// Broadcast channel capacity for shell-output fan-out.
    fn shell_broadcast_cap(&self) -> usize;

    /// Broadcast channel capacity for transfer-progress fan-out.
    fn transfer_broadcast_cap(&self) -> usize;

    /// Broadcast channel capacity for session-health fan-out.
    fn session_broadcast_cap(&self) -> usize;

    /// Broadcast channel capacity for forward-event fan-out.
    fn forward_broadcast_cap(&self) -> usize;

    /// Notification debounce window for `resources/updated`.
    fn notify_debounce(&self) -> Duration;

    /// Force-flush window for `resources/updated`.
    fn notify_force_flush(&self) -> Duration;

    /// Keepalive period for `resources/updated`.
    fn notify_keepalive(&self) -> Duration;

    /// Peer-GC scan interval for the subscription registry.
    fn peer_gc_interval(&self) -> Duration;

    /// Maximum concurrent commands per session (cap enforced by use cases).
    fn max_commands_per_session(&self) -> usize;

    /// Maximum concurrent shells per session.
    fn max_shells_per_session(&self) -> usize;

    /// Maximum concurrent transfers per session.
    fn max_transfers_per_session(&self) -> usize;
}

#[cfg(test)]
mod tests {
    use super::ConfigPort;
    use std::time::Duration;

    fn _assert_dyn_safe(_p: &dyn ConfigPort) {}

    struct StubConfig;

    impl ConfigPort for StubConfig {
        fn connect_timeout(&self) -> Duration {
            Duration::from_secs(30)
        }
        fn command_timeout(&self) -> Duration {
            Duration::from_secs(180)
        }
        fn max_retries(&self) -> u32 {
            3
        }
        fn retry_delay(&self) -> Duration {
            Duration::from_millis(1000)
        }
        fn inactivity_timeout(&self) -> Duration {
            Duration::from_secs(300)
        }
        fn compression_enabled(&self) -> bool {
            true
        }
        fn command_cleanup_ttl(&self) -> Duration {
            Duration::from_secs(60)
        }
        fn transfer_cleanup_ttl(&self) -> Duration {
            Duration::from_secs(300)
        }
        fn shell_inactivity_ttl(&self) -> Duration {
            Duration::from_secs(600)
        }
        fn shell_max_buffer_size(&self) -> u64 {
            10 * 1024 * 1024
        }
        fn command_max_buffer_size(&self) -> u64 {
            10 * 1024 * 1024
        }
        fn output_default_bytes(&self) -> usize {
            16_384
        }
        fn output_max_bytes_cap(&self) -> usize {
            1_048_576
        }
        fn list_max_items_default(&self) -> usize {
            500
        }
        fn list_max_items_cap(&self) -> usize {
            10_000
        }
        fn command_broadcast_cap(&self) -> usize {
            1024
        }
        fn shell_broadcast_cap(&self) -> usize {
            1024
        }
        fn transfer_broadcast_cap(&self) -> usize {
            256
        }
        fn session_broadcast_cap(&self) -> usize {
            64
        }
        fn forward_broadcast_cap(&self) -> usize {
            128
        }
        fn notify_debounce(&self) -> Duration {
            Duration::from_millis(20)
        }
        fn notify_force_flush(&self) -> Duration {
            Duration::from_millis(200)
        }
        fn notify_keepalive(&self) -> Duration {
            Duration::from_secs(30)
        }
        fn peer_gc_interval(&self) -> Duration {
            Duration::from_secs(15)
        }
        fn max_commands_per_session(&self) -> usize {
            100
        }
        fn max_shells_per_session(&self) -> usize {
            10
        }
        fn max_transfers_per_session(&self) -> usize {
            10
        }
    }

    #[test]
    fn stub_config_round_trip() {
        let cfg = StubConfig;
        assert_eq!(cfg.max_retries(), 3);
        assert_eq!(cfg.connect_timeout(), Duration::from_secs(30));
        assert!(cfg.compression_enabled());
        assert_eq!(cfg.max_commands_per_session(), 100);
    }
}
