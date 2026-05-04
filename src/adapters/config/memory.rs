//! Deterministic in-memory [`ConfigPort`] adapter for tests.
//!
//! [`MapConfig`] stores one explicit value per accessor in a plain
//! `#[derive(Clone)]` struct. Tests can pin values without mutating the
//! process environment (which would race other tests under
//! `cargo test`'s default thread pool).
//!
//! Use [`MapConfig::default_v3`] to start from the v3 production
//! defaults and override individual accessors with the
//! `with_*` builder-style setters. Compiled only under
//! `#[cfg(test)]` or the `test-fixtures` feature so this type never
//! appears in a release binary.

use std::time::Duration;

use crate::ports::config::ConfigPort;

/// In-memory [`ConfigPort`] backed by per-accessor fields.
#[derive(Debug, Clone)]
pub struct MapConfig {
    connect_timeout: Duration,
    command_timeout: Duration,
    max_retries: u32,
    retry_delay: Duration,
    inactivity_timeout: Duration,
    compression_enabled: bool,
    command_cleanup_ttl: Duration,
    transfer_cleanup_ttl: Duration,
    shell_inactivity_ttl: Duration,
    shell_max_buffer_size: u64,
    command_max_buffer_size: u64,
    output_default_bytes: usize,
    output_max_bytes_cap: usize,
    list_max_items_default: usize,
    list_max_items_cap: usize,
    command_broadcast_cap: usize,
    shell_broadcast_cap: usize,
    transfer_broadcast_cap: usize,
    session_broadcast_cap: usize,
    forward_broadcast_cap: usize,
    notify_debounce: Duration,
    notify_force_flush: Duration,
    notify_keepalive: Duration,
    peer_gc_interval: Duration,
    max_commands_per_session: usize,
    max_shells_per_session: usize,
    max_transfers_per_session: usize,
}

impl MapConfig {
    /// Construct a [`MapConfig`] populated with the v3 production
    /// defaults. Tests that don't care about specific values can use
    /// this directly; tests that do can chain `with_*` setters.
    #[must_use]
    pub const fn default_v3() -> Self {
        Self {
            connect_timeout: Duration::from_secs(30),
            command_timeout: Duration::from_mins(3),
            max_retries: 3,
            retry_delay: Duration::from_secs(1),
            inactivity_timeout: Duration::from_mins(5),
            compression_enabled: true,
            command_cleanup_ttl: Duration::from_mins(1),
            transfer_cleanup_ttl: Duration::from_mins(5),
            shell_inactivity_ttl: Duration::from_mins(10),
            shell_max_buffer_size: 10 * 1024 * 1024,
            command_max_buffer_size: 10 * 1024 * 1024,
            output_default_bytes: 16 * 1024,
            output_max_bytes_cap: 1024 * 1024,
            list_max_items_default: 500,
            list_max_items_cap: 10_000,
            command_broadcast_cap: 1024,
            shell_broadcast_cap: 1024,
            transfer_broadcast_cap: 256,
            session_broadcast_cap: 256,
            forward_broadcast_cap: 256,
            notify_debounce: Duration::from_millis(50),
            notify_force_flush: Duration::from_secs(1),
            notify_keepalive: Duration::from_secs(30),
            peer_gc_interval: Duration::from_secs(30),
            max_commands_per_session: 100,
            max_shells_per_session: 10,
            max_transfers_per_session: 10,
        }
    }

    // --- builder-style setters ---------------------------------------

    /// Override [`ConfigPort::connect_timeout`].
    #[must_use]
    pub const fn with_connect_timeout(mut self, value: Duration) -> Self {
        self.connect_timeout = value;
        self
    }

    /// Override [`ConfigPort::command_timeout`].
    #[must_use]
    pub const fn with_command_timeout(mut self, value: Duration) -> Self {
        self.command_timeout = value;
        self
    }

    /// Override [`ConfigPort::max_retries`].
    #[must_use]
    pub const fn with_max_retries(mut self, value: u32) -> Self {
        self.max_retries = value;
        self
    }

    /// Override [`ConfigPort::retry_delay`].
    #[must_use]
    pub const fn with_retry_delay(mut self, value: Duration) -> Self {
        self.retry_delay = value;
        self
    }

    /// Override [`ConfigPort::inactivity_timeout`].
    #[must_use]
    pub const fn with_inactivity_timeout(mut self, value: Duration) -> Self {
        self.inactivity_timeout = value;
        self
    }

    /// Override [`ConfigPort::compression_enabled`].
    #[must_use]
    pub const fn with_compression_enabled(mut self, value: bool) -> Self {
        self.compression_enabled = value;
        self
    }

    /// Override [`ConfigPort::command_cleanup_ttl`].
    #[must_use]
    pub const fn with_command_cleanup_ttl(mut self, value: Duration) -> Self {
        self.command_cleanup_ttl = value;
        self
    }

    /// Override [`ConfigPort::transfer_cleanup_ttl`].
    #[must_use]
    pub const fn with_transfer_cleanup_ttl(mut self, value: Duration) -> Self {
        self.transfer_cleanup_ttl = value;
        self
    }

    /// Override [`ConfigPort::shell_inactivity_ttl`].
    #[must_use]
    pub const fn with_shell_inactivity_ttl(mut self, value: Duration) -> Self {
        self.shell_inactivity_ttl = value;
        self
    }

    /// Override [`ConfigPort::shell_max_buffer_size`].
    #[must_use]
    pub const fn with_shell_max_buffer_size(mut self, value: u64) -> Self {
        self.shell_max_buffer_size = value;
        self
    }

    /// Override [`ConfigPort::command_max_buffer_size`].
    #[must_use]
    pub const fn with_command_max_buffer_size(mut self, value: u64) -> Self {
        self.command_max_buffer_size = value;
        self
    }

    /// Override [`ConfigPort::output_default_bytes`].
    #[must_use]
    pub const fn with_output_default_bytes(mut self, value: usize) -> Self {
        self.output_default_bytes = value;
        self
    }

    /// Override [`ConfigPort::output_max_bytes_cap`].
    #[must_use]
    pub const fn with_output_max_bytes_cap(mut self, value: usize) -> Self {
        self.output_max_bytes_cap = value;
        self
    }

    /// Override [`ConfigPort::list_max_items_default`].
    #[must_use]
    pub const fn with_list_max_items_default(mut self, value: usize) -> Self {
        self.list_max_items_default = value;
        self
    }

    /// Override [`ConfigPort::list_max_items_cap`].
    #[must_use]
    pub const fn with_list_max_items_cap(mut self, value: usize) -> Self {
        self.list_max_items_cap = value;
        self
    }

    /// Override [`ConfigPort::command_broadcast_cap`].
    #[must_use]
    pub const fn with_command_broadcast_cap(mut self, value: usize) -> Self {
        self.command_broadcast_cap = value;
        self
    }

    /// Override [`ConfigPort::shell_broadcast_cap`].
    #[must_use]
    pub const fn with_shell_broadcast_cap(mut self, value: usize) -> Self {
        self.shell_broadcast_cap = value;
        self
    }

    /// Override [`ConfigPort::transfer_broadcast_cap`].
    #[must_use]
    pub const fn with_transfer_broadcast_cap(mut self, value: usize) -> Self {
        self.transfer_broadcast_cap = value;
        self
    }

    /// Override [`ConfigPort::session_broadcast_cap`].
    #[must_use]
    pub const fn with_session_broadcast_cap(mut self, value: usize) -> Self {
        self.session_broadcast_cap = value;
        self
    }

    /// Override [`ConfigPort::forward_broadcast_cap`].
    #[must_use]
    pub const fn with_forward_broadcast_cap(mut self, value: usize) -> Self {
        self.forward_broadcast_cap = value;
        self
    }

    /// Override [`ConfigPort::notify_debounce`].
    #[must_use]
    pub const fn with_notify_debounce(mut self, value: Duration) -> Self {
        self.notify_debounce = value;
        self
    }

    /// Override [`ConfigPort::notify_force_flush`].
    #[must_use]
    pub const fn with_notify_force_flush(mut self, value: Duration) -> Self {
        self.notify_force_flush = value;
        self
    }

    /// Override [`ConfigPort::notify_keepalive`].
    #[must_use]
    pub const fn with_notify_keepalive(mut self, value: Duration) -> Self {
        self.notify_keepalive = value;
        self
    }

    /// Override [`ConfigPort::peer_gc_interval`].
    #[must_use]
    pub const fn with_peer_gc_interval(mut self, value: Duration) -> Self {
        self.peer_gc_interval = value;
        self
    }

    /// Override [`ConfigPort::max_commands_per_session`].
    #[must_use]
    pub const fn with_max_commands_per_session(mut self, value: usize) -> Self {
        self.max_commands_per_session = value;
        self
    }

    /// Override [`ConfigPort::max_shells_per_session`].
    #[must_use]
    pub const fn with_max_shells_per_session(mut self, value: usize) -> Self {
        self.max_shells_per_session = value;
        self
    }

    /// Override [`ConfigPort::max_transfers_per_session`].
    #[must_use]
    pub const fn with_max_transfers_per_session(mut self, value: usize) -> Self {
        self.max_transfers_per_session = value;
        self
    }
}

impl Default for MapConfig {
    /// Default to v3 production defaults.
    fn default() -> Self {
        Self::default_v3()
    }
}

impl ConfigPort for MapConfig {
    fn connect_timeout(&self) -> Duration {
        self.connect_timeout
    }

    fn command_timeout(&self) -> Duration {
        self.command_timeout
    }

    fn max_retries(&self) -> u32 {
        self.max_retries
    }

    fn retry_delay(&self) -> Duration {
        self.retry_delay
    }

    fn inactivity_timeout(&self) -> Duration {
        self.inactivity_timeout
    }

    fn compression_enabled(&self) -> bool {
        self.compression_enabled
    }

    fn command_cleanup_ttl(&self) -> Duration {
        self.command_cleanup_ttl
    }

    fn transfer_cleanup_ttl(&self) -> Duration {
        self.transfer_cleanup_ttl
    }

    fn shell_inactivity_ttl(&self) -> Duration {
        self.shell_inactivity_ttl
    }

    fn shell_max_buffer_size(&self) -> u64 {
        self.shell_max_buffer_size
    }

    fn command_max_buffer_size(&self) -> u64 {
        self.command_max_buffer_size
    }

    fn output_default_bytes(&self) -> usize {
        self.output_default_bytes
    }

    fn output_max_bytes_cap(&self) -> usize {
        self.output_max_bytes_cap
    }

    fn list_max_items_default(&self) -> usize {
        self.list_max_items_default
    }

    fn list_max_items_cap(&self) -> usize {
        self.list_max_items_cap
    }

    fn command_broadcast_cap(&self) -> usize {
        self.command_broadcast_cap
    }

    fn shell_broadcast_cap(&self) -> usize {
        self.shell_broadcast_cap
    }

    fn transfer_broadcast_cap(&self) -> usize {
        self.transfer_broadcast_cap
    }

    fn session_broadcast_cap(&self) -> usize {
        self.session_broadcast_cap
    }

    fn forward_broadcast_cap(&self) -> usize {
        self.forward_broadcast_cap
    }

    fn notify_debounce(&self) -> Duration {
        self.notify_debounce
    }

    fn notify_force_flush(&self) -> Duration {
        self.notify_force_flush
    }

    fn notify_keepalive(&self) -> Duration {
        self.notify_keepalive
    }

    fn peer_gc_interval(&self) -> Duration {
        self.peer_gc_interval
    }

    fn max_commands_per_session(&self) -> usize {
        self.max_commands_per_session
    }

    fn max_shells_per_session(&self) -> usize {
        self.max_shells_per_session
    }

    fn max_transfers_per_session(&self) -> usize {
        self.max_transfers_per_session
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::MapConfig;
    use crate::ports::config::ConfigPort;

    #[test]
    fn default_v3_returns_baseline() {
        let cfg = MapConfig::default_v3();
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
    fn default_matches_default_v3() {
        let auto = MapConfig::default();
        let manual = MapConfig::default_v3();
        // Spot-check: a single accessor diff would propagate to many
        // call sites, so verifying parity on representative fields
        // covers the equivalence.
        assert_eq!(auto.connect_timeout(), manual.connect_timeout());
        assert_eq!(auto.max_retries(), manual.max_retries());
        assert_eq!(auto.shell_max_buffer_size(), manual.shell_max_buffer_size());
        assert_eq!(
            auto.max_transfers_per_session(),
            manual.max_transfers_per_session()
        );
    }

    #[test]
    fn builder_setters_override_individual_fields() {
        let cfg = MapConfig::default_v3()
            .with_connect_timeout(Duration::from_secs(7))
            .with_command_timeout(Duration::from_secs(11))
            .with_max_retries(13)
            .with_retry_delay(Duration::from_millis(17))
            .with_inactivity_timeout(Duration::from_secs(19))
            .with_compression_enabled(false)
            .with_command_cleanup_ttl(Duration::from_secs(23))
            .with_transfer_cleanup_ttl(Duration::from_secs(29))
            .with_shell_inactivity_ttl(Duration::from_secs(31))
            .with_shell_max_buffer_size(37)
            .with_command_max_buffer_size(41)
            .with_output_default_bytes(43)
            .with_output_max_bytes_cap(47)
            .with_list_max_items_default(53)
            .with_list_max_items_cap(59)
            .with_command_broadcast_cap(61)
            .with_shell_broadcast_cap(67)
            .with_transfer_broadcast_cap(71)
            .with_session_broadcast_cap(73)
            .with_forward_broadcast_cap(79)
            .with_notify_debounce(Duration::from_millis(83))
            .with_notify_force_flush(Duration::from_millis(89))
            .with_notify_keepalive(Duration::from_secs(97))
            .with_peer_gc_interval(Duration::from_secs(101))
            .with_max_commands_per_session(103)
            .with_max_shells_per_session(107)
            .with_max_transfers_per_session(109);

        assert_eq!(cfg.connect_timeout(), Duration::from_secs(7));
        assert_eq!(cfg.command_timeout(), Duration::from_secs(11));
        assert_eq!(cfg.max_retries(), 13);
        assert_eq!(cfg.retry_delay(), Duration::from_millis(17));
        assert_eq!(cfg.inactivity_timeout(), Duration::from_secs(19));
        assert!(!cfg.compression_enabled());
        assert_eq!(cfg.command_cleanup_ttl(), Duration::from_secs(23));
        assert_eq!(cfg.transfer_cleanup_ttl(), Duration::from_secs(29));
        assert_eq!(cfg.shell_inactivity_ttl(), Duration::from_secs(31));
        assert_eq!(cfg.shell_max_buffer_size(), 37);
        assert_eq!(cfg.command_max_buffer_size(), 41);
        assert_eq!(cfg.output_default_bytes(), 43);
        assert_eq!(cfg.output_max_bytes_cap(), 47);
        assert_eq!(cfg.list_max_items_default(), 53);
        assert_eq!(cfg.list_max_items_cap(), 59);
        assert_eq!(cfg.command_broadcast_cap(), 61);
        assert_eq!(cfg.shell_broadcast_cap(), 67);
        assert_eq!(cfg.transfer_broadcast_cap(), 71);
        assert_eq!(cfg.session_broadcast_cap(), 73);
        assert_eq!(cfg.forward_broadcast_cap(), 79);
        assert_eq!(cfg.notify_debounce(), Duration::from_millis(83));
        assert_eq!(cfg.notify_force_flush(), Duration::from_millis(89));
        assert_eq!(cfg.notify_keepalive(), Duration::from_secs(97));
        assert_eq!(cfg.peer_gc_interval(), Duration::from_secs(101));
        assert_eq!(cfg.max_commands_per_session(), 103);
        assert_eq!(cfg.max_shells_per_session(), 107);
        assert_eq!(cfg.max_transfers_per_session(), 109);
    }

    #[test]
    fn map_config_is_dyn_safe() {
        fn _accepts_dyn(_p: &dyn ConfigPort) {}
        let cfg = MapConfig::default_v3();
        _accepts_dyn(&cfg);
    }

    #[test]
    fn clone_preserves_all_fields() {
        let original = MapConfig::default_v3()
            .with_max_retries(99)
            .with_compression_enabled(false);
        let copy = original.clone();
        assert_eq!(copy.max_retries(), 99);
        assert!(!copy.compression_enabled());
    }
}
