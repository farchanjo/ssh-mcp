//! Production [`ConfigPort`] adapter — reads from environment variables.
//!
//! Every accessor delegates to the matching helper in
//! [`crate::adapters::config::internal`] so the workspace shares a single
//! source of truth for env-var parsing. Parameter overrides are not
//! exposed through the port surface: use cases that need to override an
//! effective value pass an explicit argument to the relevant SSH
//! operation, while the port only carries the resolved configuration.
//!
//! `EnvConfig` is a zero-sized type that implements [`Copy`], so it can be
//! shared across threads via `Arc<EnvConfig>` (or simply by value) without
//! heap allocation.

use std::time::Duration;

#[cfg(feature = "port_forward")]
use crate::adapters::config::internal::resolve_forward_broadcast_cap;
use crate::adapters::config::internal::{
    resolve_command_broadcast_cap, resolve_command_cleanup_ttl, resolve_command_max_buffer_size,
    resolve_command_timeout, resolve_compression, resolve_connect_timeout,
    resolve_inactivity_timeout, resolve_list_max_items_cap, resolve_list_max_items_default,
    resolve_max_retries, resolve_notify_debounce_ms, resolve_notify_force_flush_ms,
    resolve_notify_keepalive_s, resolve_output_default_bytes, resolve_output_max_bytes_cap,
    resolve_peer_gc_interval_s, resolve_retry_delay, resolve_session_broadcast_cap,
    resolve_shell_broadcast_cap, resolve_shell_inactivity_ttl, resolve_shell_max_buffer_size,
    resolve_transfer_broadcast_cap, resolve_transfer_cleanup_ttl,
};
use crate::adapters::sftp::internal::transfer::MAX_TRANSFERS_PER_SESSION;
use crate::adapters::ssh::internal::shell::MAX_SHELLS_PER_SESSION;
use crate::ports::config::ConfigPort;

/// Production environment-variable backed configuration adapter.
///
/// Construct with [`EnvConfig::default`] (or `EnvConfig`).
#[derive(Debug, Default, Clone, Copy)]
pub struct EnvConfig;

/// Default broadcast capacity surfaced when the `port_forward` feature is
/// disabled. Mirrors
/// `crate::adapters::config::internal::DEFAULT_FORWARD_BROADCAST_CAP`,
/// which is itself feature-gated and therefore unreachable from this
/// always-compiled accessor.
#[cfg(not(feature = "port_forward"))]
const FORWARD_BROADCAST_CAP_FALLBACK: usize = 256;

/// Default cap on concurrent async commands per session. v3 has no
/// dedicated constant or resolver — async-command storage is currently
/// unbounded — so the port surfaces a sensible ceiling that use cases
/// can rely on without knowing the legacy code path. Mirrors the value
/// documented by the [`ConfigPort`] stub used in tests.
const MAX_COMMANDS_PER_SESSION_FALLBACK: usize = 100;

impl ConfigPort for EnvConfig {
    fn connect_timeout(&self) -> Duration {
        resolve_connect_timeout(None)
    }

    fn command_timeout(&self) -> Duration {
        resolve_command_timeout(None)
    }

    fn max_retries(&self) -> u32 {
        resolve_max_retries(None)
    }

    fn retry_delay(&self) -> Duration {
        resolve_retry_delay(None)
    }

    fn inactivity_timeout(&self) -> Duration {
        resolve_inactivity_timeout()
    }

    fn compression_enabled(&self) -> bool {
        resolve_compression(None)
    }

    fn command_cleanup_ttl(&self) -> Duration {
        resolve_command_cleanup_ttl()
    }

    fn transfer_cleanup_ttl(&self) -> Duration {
        resolve_transfer_cleanup_ttl()
    }

    fn shell_inactivity_ttl(&self) -> Duration {
        resolve_shell_inactivity_ttl(None)
    }

    fn shell_max_buffer_size(&self) -> u64 {
        resolve_shell_max_buffer_size(None)
    }

    fn command_max_buffer_size(&self) -> u64 {
        resolve_command_max_buffer_size()
    }

    fn output_default_bytes(&self) -> usize {
        resolve_output_default_bytes()
    }

    fn output_max_bytes_cap(&self) -> usize {
        resolve_output_max_bytes_cap()
    }

    fn list_max_items_default(&self) -> usize {
        resolve_list_max_items_default()
    }

    fn list_max_items_cap(&self) -> usize {
        resolve_list_max_items_cap()
    }

    fn command_broadcast_cap(&self) -> usize {
        resolve_command_broadcast_cap()
    }

    fn shell_broadcast_cap(&self) -> usize {
        resolve_shell_broadcast_cap()
    }

    fn transfer_broadcast_cap(&self) -> usize {
        resolve_transfer_broadcast_cap()
    }

    fn session_broadcast_cap(&self) -> usize {
        resolve_session_broadcast_cap()
    }

    fn forward_broadcast_cap(&self) -> usize {
        // The v3 resolver is gated behind the `port_forward` Cargo
        // feature because the legacy forward state only compiles under
        // it. The port surface, however, always exposes the accessor
        // so use cases stay feature-agnostic. When the feature is on
        // we delegate to the v3 resolver; otherwise we mirror the v3
        // default.
        #[cfg(feature = "port_forward")]
        {
            resolve_forward_broadcast_cap()
        }
        #[cfg(not(feature = "port_forward"))]
        {
            FORWARD_BROADCAST_CAP_FALLBACK
        }
    }

    fn notify_debounce(&self) -> Duration {
        Duration::from_millis(resolve_notify_debounce_ms())
    }

    fn notify_force_flush(&self) -> Duration {
        Duration::from_millis(resolve_notify_force_flush_ms())
    }

    fn notify_keepalive(&self) -> Duration {
        Duration::from_secs(resolve_notify_keepalive_s())
    }

    fn peer_gc_interval(&self) -> Duration {
        Duration::from_secs(resolve_peer_gc_interval_s())
    }

    fn max_commands_per_session(&self) -> usize {
        MAX_COMMANDS_PER_SESSION_FALLBACK
    }

    fn max_shells_per_session(&self) -> usize {
        MAX_SHELLS_PER_SESSION
    }

    fn max_transfers_per_session(&self) -> usize {
        MAX_TRANSFERS_PER_SESSION
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::EnvConfig;
    use crate::ports::config::ConfigPort;

    #[test]
    fn env_config_returns_v3_defaults_with_unset_env() {
        // When no env vars are set the EnvConfig must surface the same
        // defaults the v3 resolvers expose. We assert a representative
        // subset rather than every accessor; the resolver tests in
        // `adapters::config::internal` cover the full value surface.
        let cfg = EnvConfig;

        assert_eq!(cfg.connect_timeout(), Duration::from_secs(30));
        assert_eq!(cfg.command_timeout(), Duration::from_secs(180));
        assert_eq!(cfg.max_retries(), 3);
        assert_eq!(cfg.retry_delay(), Duration::from_secs(1));
        assert_eq!(cfg.inactivity_timeout(), Duration::from_secs(300));
        assert!(cfg.compression_enabled());
        assert_eq!(cfg.command_cleanup_ttl(), Duration::from_secs(60));
        assert_eq!(cfg.transfer_cleanup_ttl(), Duration::from_secs(300));
        assert_eq!(cfg.shell_inactivity_ttl(), Duration::from_secs(600));
        assert_eq!(cfg.shell_max_buffer_size(), 10 * 1024 * 1024);
        assert_eq!(cfg.command_max_buffer_size(), 10 * 1024 * 1024);
        assert_eq!(cfg.output_default_bytes(), 16_384);
        assert_eq!(cfg.output_max_bytes_cap(), 1_048_576);
        assert_eq!(cfg.list_max_items_default(), 500);
        assert_eq!(cfg.list_max_items_cap(), 10_000);
        assert_eq!(cfg.command_broadcast_cap(), 1024);
        assert_eq!(cfg.shell_broadcast_cap(), 1024);
        assert_eq!(cfg.transfer_broadcast_cap(), 256);
        assert_eq!(cfg.session_broadcast_cap(), 256);
        assert_eq!(cfg.forward_broadcast_cap(), 256);
        assert_eq!(cfg.notify_debounce(), Duration::from_millis(50));
        assert_eq!(cfg.notify_force_flush(), Duration::from_secs(1));
        assert_eq!(cfg.notify_keepalive(), Duration::from_secs(30));
        assert_eq!(cfg.peer_gc_interval(), Duration::from_secs(30));
        assert_eq!(cfg.max_commands_per_session(), 100);
        assert_eq!(cfg.max_shells_per_session(), 10);
        assert_eq!(cfg.max_transfers_per_session(), 10);
    }

    #[test]
    fn env_config_is_dyn_safe() {
        fn _accepts_dyn(_p: &dyn ConfigPort) {}
        let cfg = EnvConfig;
        _accepts_dyn(&cfg);
    }

    #[test]
    fn env_config_is_zero_sized_and_copy() {
        // Sanity check that EnvConfig stays a ZST so it can be shared by
        // value without `Arc<>` indirection.
        assert_eq!(std::mem::size_of::<EnvConfig>(), 0);
        let one = EnvConfig;
        let two = one;
        let three = one;
        // Touch all three to prove `Copy` semantics — moving `one` would
        // be a compile error if `Copy` were not implemented.
        assert_eq!(one.max_retries(), two.max_retries());
        assert_eq!(two.max_retries(), three.max_retries());
    }
}
