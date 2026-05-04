//! Configuration resolution for SSH MCP server.
//!
//! This module handles configuration values with a three-tier priority system:
//!
//! 1. **Parameter** - Explicitly provided function parameter (highest priority)
//! 2. **Environment Variable** - Value from environment variable
//! 3. **Default** - Built-in default value (lowest priority)
//!
//! # Environment Variables
//!
//! | Variable | Default | Description |
//! |----------|---------|-------------|
//! | `SSH_CONNECT_TIMEOUT` | 30s | Connection timeout in seconds |
//! | `SSH_COMMAND_TIMEOUT` | 180s | Command execution timeout in seconds |
//! | `SSH_MAX_RETRIES` | 3 | Maximum retry attempts |
//! | `SSH_RETRY_DELAY_MS` | 1000ms | Initial retry delay in milliseconds |
//! | `SSH_INACTIVITY_TIMEOUT` | 300s | Session inactivity timeout in seconds |
//! | `SSH_COMPRESSION` | true | Enable zlib compression |
//! | `SSH_COMMAND_CLEANUP_TTL` | 60s | TTL before unread command output is cleaned up |
//! | `SSH_SHELL_INACTIVITY_TTL` | 600s | Shell auto-close after inactivity |
//! | `SSH_SHELL_MAX_BUFFER_SIZE` | 10m | Max shell output buffer (supports b/k/m/g/t) |

use std::env;
use std::time::Duration;

use crate::domain::subscription::LagPolicy;

/// Default SSH connection timeout
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Default SSH command execution timeout
pub const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_mins(3);

/// Default maximum retry attempts for SSH connection
pub const DEFAULT_MAX_RETRIES: u32 = 3;

/// Default retry delay
pub const DEFAULT_RETRY_DELAY: Duration = Duration::from_secs(1);

/// Default session inactivity timeout (separate from connect timeout)
pub const DEFAULT_INACTIVITY_TIMEOUT: Duration = Duration::from_mins(5);

/// Maximum retry delay cap (10 seconds)
pub const MAX_RETRY_DELAY: Duration = Duration::from_secs(10);

/// Environment variable name for SSH connection timeout
pub const CONNECT_TIMEOUT_ENV_VAR: &str = "SSH_CONNECT_TIMEOUT";

/// Environment variable name for SSH command execution timeout
pub const COMMAND_TIMEOUT_ENV_VAR: &str = "SSH_COMMAND_TIMEOUT";

/// Environment variable name for SSH max retries
pub const MAX_RETRIES_ENV_VAR: &str = "SSH_MAX_RETRIES";

/// Environment variable name for SSH retry delay in milliseconds
pub const RETRY_DELAY_MS_ENV_VAR: &str = "SSH_RETRY_DELAY_MS";

/// Environment variable name for SSH session inactivity timeout
pub const INACTIVITY_TIMEOUT_ENV_VAR: &str = "SSH_INACTIVITY_TIMEOUT";

/// Environment variable name for SSH compression
pub const COMPRESSION_ENV_VAR: &str = "SSH_COMPRESSION";

/// Default TTL for completed command cleanup (seconds)
pub const DEFAULT_COMMAND_CLEANUP_TTL: Duration = Duration::from_mins(1);

/// Environment variable name for command cleanup TTL
pub const COMMAND_CLEANUP_TTL_ENV_VAR: &str = "SSH_COMMAND_CLEANUP_TTL";

/// Default shell inactivity timeout (seconds) — auto-close after no read/write
pub const DEFAULT_SHELL_INACTIVITY_TTL: Duration = Duration::from_mins(10);

/// Environment variable name for shell inactivity TTL
pub const SHELL_INACTIVITY_TTL_ENV_VAR: &str = "SSH_SHELL_INACTIVITY_TTL";

/// Default shell output buffer max size (10 MB)
pub const DEFAULT_SHELL_MAX_BUFFER_SIZE: u64 = 10 * 1024 * 1024;

/// Default maximum per-command output buffer size (stdout + stderr each) in bytes.
/// Exceeding this causes oldest bytes to be drained head-first, bounding RAM usage.
pub const DEFAULT_COMMAND_MAX_BUFFER_SIZE: u64 = 10 * 1024 * 1024;

/// Default TTL (in seconds) before a terminal (completed/failed/cancelled)
/// transfer is removed from storage. Gives LLM a chance to poll final state.
pub const DEFAULT_TRANSFER_CLEANUP_TTL_SECS: u64 = 300;

/// Default `max_output_bytes` applied to output-returning tools when the
/// caller does not pass an explicit value.
pub const DEFAULT_OUTPUT_MAX_BYTES: usize = 16 * 1024;
/// Hard cap on `max_output_bytes` regardless of caller request.
pub const DEFAULT_OUTPUT_MAX_BYTES_CAP: usize = 1024 * 1024;
/// Default `max_items` applied to list-returning tools when the caller
/// does not pass an explicit value.
pub const DEFAULT_LIST_MAX_ITEMS: usize = 500;
/// Hard cap on `max_items`.
pub const DEFAULT_LIST_MAX_ITEMS_CAP: usize = 10_000;

/// Default broadcast channel capacity for per-command output chunks.
pub const DEFAULT_COMMAND_BROADCAST_CAP: usize = 1024;
/// Hard cap on the broadcast channel capacity.
pub const COMMAND_BROADCAST_CAP_MAX: usize = 65_536;
/// Floor for the broadcast channel capacity.
pub const COMMAND_BROADCAST_CAP_MIN: usize = 16;

/// Default broadcast channel capacity for per-shell output chunks.
pub const DEFAULT_SHELL_BROADCAST_CAP: usize = 1024;
/// Hard cap on the per-shell broadcast channel capacity.
pub const SHELL_BROADCAST_CAP_MAX: usize = 65_536;
/// Floor for the per-shell broadcast channel capacity.
pub const SHELL_BROADCAST_CAP_MIN: usize = 16;

/// Default broadcast channel capacity for per-transfer progress events.
pub const DEFAULT_TRANSFER_BROADCAST_CAP: usize = 256;
/// Hard cap on the per-transfer broadcast channel capacity.
pub const TRANSFER_BROADCAST_CAP_MAX: usize = 4096;
/// Floor for the per-transfer broadcast channel capacity.
pub const TRANSFER_BROADCAST_CAP_MIN: usize = 8;

/// Default broadcast channel capacity for per-session health events.
pub const DEFAULT_SESSION_BROADCAST_CAP: usize = 256;
/// Hard cap on the per-session broadcast channel capacity.
pub const SESSION_BROADCAST_CAP_MAX: usize = 4096;
/// Floor for the per-session broadcast channel capacity.
pub const SESSION_BROADCAST_CAP_MIN: usize = 8;

/// Default broadcast channel capacity for per-forwarder events.
#[cfg(feature = "port_forward")]
pub const DEFAULT_FORWARD_BROADCAST_CAP: usize = 256;
/// Hard cap on the per-forwarder broadcast channel capacity.
#[cfg(feature = "port_forward")]
pub const FORWARD_BROADCAST_CAP_MAX: usize = 4096;
/// Floor for the per-forwarder broadcast channel capacity.
#[cfg(feature = "port_forward")]
pub const FORWARD_BROADCAST_CAP_MIN: usize = 8;

/// Default debounce window (milliseconds) for the subscription registry.
pub const DEFAULT_NOTIFY_DEBOUNCE_MS: u64 = 50;
/// Floor for the debounce window.
pub const NOTIFY_DEBOUNCE_MS_MIN: u64 = 5;
/// Cap for the debounce window.
pub const NOTIFY_DEBOUNCE_MS_MAX: u64 = 5_000;

/// Default force-flush interval (milliseconds) for the subscription registry.
pub const DEFAULT_NOTIFY_FORCE_FLUSH_MS: u64 = 1_000;
/// Floor for the force-flush interval.
pub const NOTIFY_FORCE_FLUSH_MS_MIN: u64 = 100;
/// Cap for the force-flush interval.
pub const NOTIFY_FORCE_FLUSH_MS_MAX: u64 = 60_000;

/// Default keepalive interval (seconds) for the subscription registry.
pub const DEFAULT_NOTIFY_KEEPALIVE_S: u64 = 30;
/// Floor for the keepalive interval.
pub const NOTIFY_KEEPALIVE_S_MIN: u64 = 5;
/// Cap for the keepalive interval.
pub const NOTIFY_KEEPALIVE_S_MAX: u64 = 300;

/// v5 Phase 2 — default per-`SubId` lane mpsc capacity.
pub const DEFAULT_LANE_BUFFER: usize = 1024;
/// v5 Phase 2 — floor for the lane mpsc capacity.
pub const LANE_BUFFER_MIN: usize = 16;
/// v5 Phase 2 — cap for the lane mpsc capacity.
pub const LANE_BUFFER_MAX: usize = 65_536;

/// v5 Phase 2 — default `ChannelMux` outbound mpsc capacity.
pub const DEFAULT_MUX_BUFFER: usize = 8_192;
/// v5 Phase 2 — floor for the `ChannelMux` outbound mpsc capacity.
pub const MUX_BUFFER_MIN: usize = 64;
/// v5 Phase 2 — cap for the `ChannelMux` outbound mpsc capacity.
pub const MUX_BUFFER_MAX: usize = 1_048_576;

/// v5 Phase 2 — default per-URI subscription cap.
pub const DEFAULT_MAX_SUBS_PER_URI: u16 = 16;
/// v5 Phase 2 — minimum per-URI subscription cap.
pub const MAX_SUBS_PER_URI_MIN: u16 = 1;

/// v5 Phase 2 — default global subscription cap.
pub const DEFAULT_MAX_SUBS_TOTAL: u16 = 1_024;
/// v5 Phase 2 — minimum global subscription cap.
pub const MAX_SUBS_TOTAL_MIN: u16 = 1;

/// v5 Phase 2 — default `BlockSlow` timeout (milliseconds).
pub const DEFAULT_BP_BLOCK_TIMEOUT_MS: u64 = 5_000;
/// v5 Phase 2 — floor for the `BlockSlow` timeout.
pub const BP_BLOCK_TIMEOUT_MS_MIN: u64 = 100;
/// v5 Phase 2 — cap for the `BlockSlow` timeout.
pub const BP_BLOCK_TIMEOUT_MS_MAX: u64 = 600_000;

/// v5 Phase 2 — default filter regex char-length cap.
pub const DEFAULT_FILTER_REGEX_MAX: usize = 1_024;
/// v5 Phase 2 — minimum filter regex cap (must allow at least the
/// shortest sensible pattern).
pub const FILTER_REGEX_MAX_MIN: usize = 16;
/// v5 Phase 2 — cap for the filter regex char-length.
pub const FILTER_REGEX_MAX_CAP: usize = 65_536;

/// v5 Phase 2 — default replay window (bytes) for `ssh_sub_replay`.
pub const DEFAULT_REPLAY_WINDOW_BYTES: usize = 1_048_576;
/// v5 Phase 2 — floor for the replay window (bytes).
pub const REPLAY_WINDOW_BYTES_MIN: usize = 4_096;
/// v5 Phase 2 — cap for the replay window (bytes).
pub const REPLAY_WINDOW_BYTES_MAX: usize = 64 * 1_048_576;

/// Default interval (seconds) at which the binary scans the subscription
/// registry for peers whose transport has closed.
pub const DEFAULT_PEER_GC_INTERVAL_S: u64 = 30;
/// Floor for the peer GC interval (seconds).
pub const PEER_GC_INTERVAL_S_MIN: u64 = 5;
/// Cap for the peer GC interval (seconds).
pub const PEER_GC_INTERVAL_S_MAX: u64 = 300;

/// Environment variable name for the subscription debounce window.
pub const NOTIFY_DEBOUNCE_MS_ENV_VAR: &str = "SSH_NOTIFY_DEBOUNCE_MS";
/// Environment variable name for the subscription force-flush interval.
pub const NOTIFY_FORCE_FLUSH_MS_ENV_VAR: &str = "SSH_NOTIFY_FORCE_FLUSH_MS";
/// Environment variable name for the subscription keepalive interval.
pub const NOTIFY_KEEPALIVE_S_ENV_VAR: &str = "SSH_NOTIFY_KEEPALIVE_S";
/// Environment variable name for the binary peer-GC scan interval.
pub const PEER_GC_INTERVAL_S_ENV_VAR: &str = "SSH_MCP_PEER_GC_INTERVAL_S";

/// v5 Phase 2 — env var for the per-`SubId` lane mpsc capacity.
pub const LANE_BUFFER_ENV_VAR: &str = "SSH_LANE_BUFFER";
/// v5 Phase 2 — env var for the `ChannelMux` outbound mpsc capacity.
pub const MUX_BUFFER_ENV_VAR: &str = "SSH_MUX_BUFFER";
/// v5 Phase 2 — env var for the per-URI subscription cap.
pub const MAX_SUBS_PER_URI_ENV_VAR: &str = "SSH_MAX_SUBS_PER_URI";
/// v5 Phase 2 — env var for the global subscription cap.
pub const MAX_SUBS_TOTAL_ENV_VAR: &str = "SSH_MAX_SUBS_TOTAL";
/// v5 Phase 2 — env var for the default lag policy.
pub const LAG_POLICY_DEFAULT_ENV_VAR: &str = "SSH_LAG_POLICY_DEFAULT";
/// v5 Phase 2 — env var for the `BlockSlow` timeout.
pub const BP_BLOCK_TIMEOUT_MS_ENV_VAR: &str = "SSH_BP_BLOCK_TIMEOUT_MS";
/// v5 Phase 2 — env var for the filter regex char cap.
pub const FILTER_REGEX_MAX_ENV_VAR: &str = "SSH_FILTER_REGEX_MAX";
/// v5 Phase 2 — env var for the replay window (bytes).
pub const REPLAY_WINDOW_BYTES_ENV_VAR: &str = "SSH_REPLAY_WINDOW_BYTES";

/// Environment variable name for shell output buffer max size
pub const SHELL_MAX_BUFFER_SIZE_ENV_VAR: &str = "SSH_SHELL_MAX_BUFFER_SIZE";
/// Environment variable name for the per-command output buffer cap.
pub const COMMAND_MAX_BUFFER_SIZE_ENV_VAR: &str = "SSH_COMMAND_MAX_BUFFER_SIZE";
/// Environment variable for the transfer cleanup TTL.
pub const TRANSFER_CLEANUP_TTL_ENV_VAR: &str = "SSH_TRANSFER_CLEANUP_TTL";
/// Environment variable for the default `max_output_bytes` applied to the
/// render layer of output-returning tools.
pub const OUTPUT_DEFAULT_BYTES_ENV_VAR: &str = "SSH_MCP_OUTPUT_DEFAULT_BYTES";
/// Environment variable for the hard cap on `max_output_bytes`.
pub const OUTPUT_MAX_BYTES_CAP_ENV_VAR: &str = "SSH_MCP_OUTPUT_MAX_BYTES_CAP";
/// Environment variable for the default `max_items` returned by list tools.
pub const LIST_MAX_ITEMS_ENV_VAR: &str = "SSH_MCP_LIST_MAX_ITEMS";
/// Environment variable for the hard cap on `max_items`.
pub const LIST_MAX_ITEMS_CAP_ENV_VAR: &str = "SSH_MCP_LIST_MAX_ITEMS_CAP";
/// Environment variable for the per-command broadcast channel capacity.
pub const COMMAND_BROADCAST_CAP_ENV_VAR: &str = "SSH_COMMAND_BROADCAST_CAP";
/// Environment variable for the per-shell broadcast channel capacity.
pub const SHELL_BROADCAST_CAP_ENV_VAR: &str = "SSH_SHELL_BROADCAST_CAP";
/// Environment variable for the per-transfer broadcast channel capacity.
pub const TRANSFER_BROADCAST_CAP_ENV_VAR: &str = "SSH_TRANSFER_BROADCAST_CAP";
/// Environment variable for the per-session health broadcast channel capacity.
pub const SESSION_BROADCAST_CAP_ENV_VAR: &str = "SSH_SESSION_BROADCAST_CAP";
/// Environment variable for the per-forwarder events broadcast channel capacity.
#[cfg(feature = "port_forward")]
pub const FORWARD_BROADCAST_CAP_ENV_VAR: &str = "SSH_FORWARD_BROADCAST_CAP";

/// Resolve the connection timeout value with priority: parameter -> env var -> default
#[must_use]
pub fn resolve_connect_timeout(timeout_param: Option<u64>) -> Duration {
    // Priority 1: Use parameter if provided
    if let Some(timeout) = timeout_param {
        return Duration::from_secs(timeout);
    }

    // Priority 2: Use environment variable if set
    if let Ok(env_timeout) = env::var(CONNECT_TIMEOUT_ENV_VAR)
        && let Ok(timeout) = env_timeout.parse::<u64>()
    {
        return Duration::from_secs(timeout);
    }

    // Priority 3: Default value
    DEFAULT_CONNECT_TIMEOUT
}

/// Resolve the command execution timeout value with priority: parameter -> env var -> default
#[must_use]
pub fn resolve_command_timeout(timeout_param: Option<u64>) -> Duration {
    // Priority 1: Use parameter if provided
    if let Some(timeout) = timeout_param {
        return Duration::from_secs(timeout);
    }

    // Priority 2: Use environment variable if set
    if let Ok(env_timeout) = env::var(COMMAND_TIMEOUT_ENV_VAR)
        && let Ok(timeout) = env_timeout.parse::<u64>()
    {
        return Duration::from_secs(timeout);
    }

    // Priority 3: Default value
    DEFAULT_COMMAND_TIMEOUT
}

/// Resolve the max retries value with priority: parameter -> env var -> default
#[must_use]
pub fn resolve_max_retries(max_retries_param: Option<u32>) -> u32 {
    // Priority 1: Use parameter if provided
    if let Some(max_retries) = max_retries_param {
        return max_retries;
    }

    // Priority 2: Use environment variable if set
    if let Ok(env_retries) = env::var(MAX_RETRIES_ENV_VAR)
        && let Ok(retries) = env_retries.parse::<u32>()
    {
        return retries;
    }

    // Priority 3: Default value
    DEFAULT_MAX_RETRIES
}

/// Resolve the retry delay value with priority: parameter -> env var -> default
#[must_use]
pub fn resolve_retry_delay(retry_delay_param: Option<u64>) -> Duration {
    // Priority 1: Use parameter if provided (milliseconds)
    if let Some(delay) = retry_delay_param {
        return Duration::from_millis(delay);
    }

    // Priority 2: Use environment variable if set (milliseconds)
    if let Ok(env_delay) = env::var(RETRY_DELAY_MS_ENV_VAR)
        && let Ok(delay) = env_delay.parse::<u64>()
    {
        return Duration::from_millis(delay);
    }

    // Priority 3: Default value
    DEFAULT_RETRY_DELAY
}

/// Resolve the inactivity timeout with priority: env var -> default (300s)
#[must_use]
pub fn resolve_inactivity_timeout() -> Duration {
    if let Ok(env_timeout) = env::var(INACTIVITY_TIMEOUT_ENV_VAR)
        && let Ok(timeout) = env_timeout.parse::<u64>()
    {
        return Duration::from_secs(timeout);
    }

    DEFAULT_INACTIVITY_TIMEOUT
}

/// Resolve the compression setting with priority: parameter -> env var -> default (true)
#[must_use]
pub fn resolve_compression(compress_param: Option<bool>) -> bool {
    // Priority 1: Use parameter if provided
    if let Some(compress) = compress_param {
        return compress;
    }

    // Priority 2: Use environment variable if set
    if let Ok(env_compress) = env::var(COMPRESSION_ENV_VAR) {
        return env_compress.eq_ignore_ascii_case("true") || env_compress == "1";
    }

    // Priority 3: Default value (enabled)
    true
}

/// Resolve the command cleanup TTL with priority: env var -> default (60s)
///
/// Controls how long completed commands remain in storage when their output
/// has not been read. Once output is read, commands are cleaned up immediately.
#[must_use]
pub fn resolve_command_cleanup_ttl() -> Duration {
    if let Ok(env_ttl) = env::var(COMMAND_CLEANUP_TTL_ENV_VAR)
        && let Ok(ttl) = env_ttl.parse::<u64>()
    {
        return Duration::from_secs(ttl);
    }

    DEFAULT_COMMAND_CLEANUP_TTL
}

/// Parse a human-readable byte size string with unit suffixes.
///
/// Supports case-insensitive suffixes:
/// - `b` or no suffix: bytes
/// - `k` / `kb`: kilobytes (×1024)
/// - `m` / `mb`: megabytes (×1024²)
/// - `g` / `gb`: gigabytes (×1024³)
/// - `t` / `tb`: terabytes (×1024⁴)
///
/// Examples: `"512k"`, `"10m"`, `"1g"`, `"1024"`, `"500mb"`, `"2tb"`
#[must_use]
pub fn parse_byte_size(input: &str) -> Option<u64> {
    let input = input.trim().to_ascii_lowercase();
    if input.is_empty() {
        return None;
    }

    let (num_part, multiplier) = parse_suffix(&input);
    num_part
        .trim()
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(multiplier))
}

/// Extract the numeric part and byte multiplier from a size string.
fn parse_suffix(input: &str) -> (&str, u64) {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    const GB: u64 = 1024 * 1024 * 1024;
    const TB: u64 = 1024 * 1024 * 1024 * 1024;

    // Check two-char suffixes first, then single-char
    let suffixes: &[(&str, u64)] = &[("tb", TB), ("gb", GB), ("mb", MB), ("kb", KB)];

    for (suffix, mult) in suffixes {
        if let Some(n) = input.strip_suffix(suffix) {
            return (n, *mult);
        }
    }

    let single_suffixes: &[(&str, u64)] = &[("t", TB), ("g", GB), ("m", MB), ("k", KB), ("b", 1)];

    for (suffix, mult) in single_suffixes {
        if let Some(n) = input.strip_suffix(suffix) {
            return (n, *mult);
        }
    }

    (input, 1)
}

/// Resolve the shell inactivity TTL with priority: parameter -> env var -> default (600s)
///
/// Controls how long an idle shell (no read/write) stays open before auto-close.
#[must_use]
pub fn resolve_shell_inactivity_ttl(ttl_param: Option<u64>) -> Duration {
    // Priority 1: Use parameter if provided
    if let Some(ttl) = ttl_param {
        return Duration::from_secs(ttl);
    }

    // Priority 2: Use environment variable if set
    if let Ok(env_ttl) = env::var(SHELL_INACTIVITY_TTL_ENV_VAR)
        && let Ok(ttl) = env_ttl.parse::<u64>()
    {
        return Duration::from_secs(ttl);
    }

    // Priority 3: Default value
    DEFAULT_SHELL_INACTIVITY_TTL
}

/// Resolve the shell output buffer max size with priority: parameter -> env var -> default (10m)
///
/// Accepts human-readable byte sizes (e.g., `"512k"`, `"10m"`, `"1g"`).
/// When the buffer exceeds this size, oldest output is truncated.
#[must_use]
pub fn resolve_shell_max_buffer_size(size_param: Option<&str>) -> u64 {
    // Priority 1: Use parameter if provided
    if let Some(size_str) = size_param
        && let Some(size) = parse_byte_size(size_str)
    {
        return size;
    }

    // Priority 2: Use environment variable if set
    if let Ok(env_size) = env::var(SHELL_MAX_BUFFER_SIZE_ENV_VAR)
        && let Some(size) = parse_byte_size(&env_size)
    {
        return size;
    }

    // Priority 3: Default value
    DEFAULT_SHELL_MAX_BUFFER_SIZE
}

/// Resolve the per-command output buffer max size. Mirrors the shell cap
/// resolver but reads `SSH_COMMAND_MAX_BUFFER_SIZE` and defaults to 10 MiB.
///
/// Applies independently to stdout and stderr. When exceeded, the oldest
/// bytes are drained head-first so the buffer stays bounded even for
/// long-running commands that produce unbounded output.
#[must_use]
pub fn resolve_command_max_buffer_size() -> u64 {
    if let Ok(env_size) = env::var(COMMAND_MAX_BUFFER_SIZE_ENV_VAR)
        && let Some(size) = parse_byte_size(&env_size)
    {
        return size;
    }
    DEFAULT_COMMAND_MAX_BUFFER_SIZE
}

/// Resolve the transfer cleanup TTL. Reads `SSH_TRANSFER_CLEANUP_TTL`
/// as an integer number of seconds; defaults to 300s (5 minutes).
///
/// Applied per-transfer: once a transfer reaches a terminal state
/// (Completed / Failed / Cancelled), a background task removes it from
/// storage after the TTL elapses.
#[must_use]
pub fn resolve_transfer_cleanup_ttl() -> Duration {
    let secs = env::var(TRANSFER_CLEANUP_TTL_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_TRANSFER_CLEANUP_TTL_SECS);
    Duration::from_secs(secs)
}

/// Resolve the effective `max_output_bytes` for output rendering.
///
/// Priority: caller parameter -> `SSH_MCP_OUTPUT_DEFAULT_BYTES` -> 16 KiB.
/// Then clamped to `resolve_output_max_bytes_cap()` (hard cap).
#[must_use]
pub fn resolve_output_default_bytes() -> usize {
    env::var(OUTPUT_DEFAULT_BYTES_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_OUTPUT_MAX_BYTES)
}

/// Resolve the hard cap on `max_output_bytes` from env (default 1 MiB).
#[must_use]
pub fn resolve_output_max_bytes_cap() -> usize {
    env::var(OUTPUT_MAX_BYTES_CAP_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_OUTPUT_MAX_BYTES_CAP)
}

/// Resolve the default `max_items` for list tools.
#[must_use]
pub fn resolve_list_max_items_default() -> usize {
    env::var(LIST_MAX_ITEMS_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_LIST_MAX_ITEMS)
}

/// Resolve the hard cap on `max_items`.
#[must_use]
pub fn resolve_list_max_items_cap() -> usize {
    env::var(LIST_MAX_ITEMS_CAP_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_LIST_MAX_ITEMS_CAP)
}

/// Resolve the broadcast channel capacity for per-command output chunks.
///
/// Reads `SSH_COMMAND_BROADCAST_CAP` (default 1024) and clamps the value to
/// `[COMMAND_BROADCAST_CAP_MIN, COMMAND_BROADCAST_CAP_MAX]`.
#[must_use]
pub fn resolve_command_broadcast_cap() -> usize {
    let raw = env::var(COMMAND_BROADCAST_CAP_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_COMMAND_BROADCAST_CAP);
    raw.clamp(COMMAND_BROADCAST_CAP_MIN, COMMAND_BROADCAST_CAP_MAX)
}

/// Resolve the broadcast channel capacity for per-shell output chunks.
///
/// Reads `SSH_SHELL_BROADCAST_CAP` (default 1024) and clamps the value to
/// `[SHELL_BROADCAST_CAP_MIN, SHELL_BROADCAST_CAP_MAX]`. Mirrors
/// [`resolve_command_broadcast_cap`] so PTY subscribers and async-command
/// subscribers share the same operational envelope.
#[must_use]
pub fn resolve_shell_broadcast_cap() -> usize {
    let raw = env::var(SHELL_BROADCAST_CAP_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_SHELL_BROADCAST_CAP);
    raw.clamp(SHELL_BROADCAST_CAP_MIN, SHELL_BROADCAST_CAP_MAX)
}

/// Resolve the broadcast channel capacity for per-transfer progress events.
///
/// Reads `SSH_TRANSFER_BROADCAST_CAP` (default 256) and clamps the value to
/// `[TRANSFER_BROADCAST_CAP_MIN, TRANSFER_BROADCAST_CAP_MAX]`. Used by
/// `RunningTransfer::new` so SFTP progress subscribers share a sane envelope.
#[must_use]
pub fn resolve_transfer_broadcast_cap() -> usize {
    let raw = env::var(TRANSFER_BROADCAST_CAP_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_TRANSFER_BROADCAST_CAP);
    raw.clamp(TRANSFER_BROADCAST_CAP_MIN, TRANSFER_BROADCAST_CAP_MAX)
}

/// Resolve the broadcast channel capacity for per-session health events.
///
/// Reads `SSH_SESSION_BROADCAST_CAP` (default 256) and clamps the value to
/// `[SESSION_BROADCAST_CAP_MIN, SESSION_BROADCAST_CAP_MAX]`. Used by
/// `SessionStorage::insert` so future `session://<id>/health` resource
/// subscribers share a sane envelope.
#[must_use]
pub fn resolve_session_broadcast_cap() -> usize {
    let raw = env::var(SESSION_BROADCAST_CAP_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_SESSION_BROADCAST_CAP);
    raw.clamp(SESSION_BROADCAST_CAP_MIN, SESSION_BROADCAST_CAP_MAX)
}

/// Resolve the broadcast channel capacity for per-forwarder events.
///
/// Reads `SSH_FORWARD_BROADCAST_CAP` (default 256) and clamps the value to
/// `[FORWARD_BROADCAST_CAP_MIN, FORWARD_BROADCAST_CAP_MAX]`. Used by the
/// port-forward state so future `forward://<id>/events` resource subscribers
/// share a sane envelope.
#[cfg(feature = "port_forward")]
#[must_use]
pub fn resolve_forward_broadcast_cap() -> usize {
    let raw = env::var(FORWARD_BROADCAST_CAP_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_FORWARD_BROADCAST_CAP);
    raw.clamp(FORWARD_BROADCAST_CAP_MIN, FORWARD_BROADCAST_CAP_MAX)
}

/// Resolve the subscription debounce window (milliseconds).
///
/// Reads `SSH_NOTIFY_DEBOUNCE_MS` (default 50) and clamps to
/// `[NOTIFY_DEBOUNCE_MS_MIN, NOTIFY_DEBOUNCE_MS_MAX]`.
#[must_use]
pub fn resolve_notify_debounce_ms() -> u64 {
    let raw = env::var(NOTIFY_DEBOUNCE_MS_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_NOTIFY_DEBOUNCE_MS);
    raw.clamp(NOTIFY_DEBOUNCE_MS_MIN, NOTIFY_DEBOUNCE_MS_MAX)
}

/// Resolve the subscription force-flush interval (milliseconds).
///
/// Reads `SSH_NOTIFY_FORCE_FLUSH_MS` (default 1000) and clamps to
/// `[NOTIFY_FORCE_FLUSH_MS_MIN, NOTIFY_FORCE_FLUSH_MS_MAX]`.
#[must_use]
pub fn resolve_notify_force_flush_ms() -> u64 {
    let raw = env::var(NOTIFY_FORCE_FLUSH_MS_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_NOTIFY_FORCE_FLUSH_MS);
    raw.clamp(NOTIFY_FORCE_FLUSH_MS_MIN, NOTIFY_FORCE_FLUSH_MS_MAX)
}

/// Resolve the subscription keepalive interval (seconds).
///
/// Reads `SSH_NOTIFY_KEEPALIVE_S` (default 30) and clamps to
/// `[NOTIFY_KEEPALIVE_S_MIN, NOTIFY_KEEPALIVE_S_MAX]`.
#[must_use]
pub fn resolve_notify_keepalive_s() -> u64 {
    let raw = env::var(NOTIFY_KEEPALIVE_S_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_NOTIFY_KEEPALIVE_S);
    raw.clamp(NOTIFY_KEEPALIVE_S_MIN, NOTIFY_KEEPALIVE_S_MAX)
}

/// Resolve the peer-GC scan interval (seconds).
///
/// The binary entry points (`ssh-mcp` and `ssh-mcp-stdio`) spawn a periodic
/// task that walks `SUBSCRIPTION_REGISTRY` and drops peers whose rmcp
/// transport has closed. This resolver reads `SSH_MCP_PEER_GC_INTERVAL_S`
/// (default 30) and clamps to `[PEER_GC_INTERVAL_S_MIN, PEER_GC_INTERVAL_S_MAX]`.
#[must_use]
pub fn resolve_peer_gc_interval_s() -> u64 {
    let raw = env::var(PEER_GC_INTERVAL_S_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_PEER_GC_INTERVAL_S);
    raw.clamp(PEER_GC_INTERVAL_S_MIN, PEER_GC_INTERVAL_S_MAX)
}

// ---------------------------------------------------------------------------
// v5 Phase 2 — Channel Mux + SubId resolvers
// ---------------------------------------------------------------------------

/// Resolve the per-`SubId` lane mpsc capacity (`SSH_LANE_BUFFER`).
#[must_use]
pub fn resolve_lane_buffer() -> usize {
    env::var(LANE_BUFFER_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_LANE_BUFFER)
        .clamp(LANE_BUFFER_MIN, LANE_BUFFER_MAX)
}

/// Resolve the `ChannelMux` outbound mpsc capacity (`SSH_MUX_BUFFER`).
#[must_use]
pub fn resolve_mux_buffer() -> usize {
    env::var(MUX_BUFFER_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MUX_BUFFER)
        .clamp(MUX_BUFFER_MIN, MUX_BUFFER_MAX)
}

/// Resolve the per-URI subscription cap (`SSH_MAX_SUBS_PER_URI`).
#[must_use]
pub fn resolve_max_subs_per_uri() -> u16 {
    env::var(MAX_SUBS_PER_URI_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(DEFAULT_MAX_SUBS_PER_URI)
        .max(MAX_SUBS_PER_URI_MIN)
}

/// Resolve the global subscription cap (`SSH_MAX_SUBS_TOTAL`).
#[must_use]
pub fn resolve_max_subs_total() -> u16 {
    env::var(MAX_SUBS_TOTAL_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(DEFAULT_MAX_SUBS_TOTAL)
        .max(MAX_SUBS_TOTAL_MIN)
}

/// Resolve the default per-`SubId` lag policy
/// (`SSH_LAG_POLICY_DEFAULT`). Falls back to
/// [`crate::domain::subscription::LagPolicy::Snapshot`] when unset
/// or unparseable.
#[must_use]
pub fn resolve_lag_policy_default() -> LagPolicy {
    env::var(LAG_POLICY_DEFAULT_ENV_VAR)
        .ok()
        .and_then(|v| LagPolicy::from_str_opt(&v))
        .unwrap_or_default()
}

/// Resolve the `BlockSlow` producer timeout (`SSH_BP_BLOCK_TIMEOUT_MS`).
#[must_use]
pub fn resolve_bp_block_timeout_ms() -> u64 {
    env::var(BP_BLOCK_TIMEOUT_MS_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_BP_BLOCK_TIMEOUT_MS)
        .clamp(BP_BLOCK_TIMEOUT_MS_MIN, BP_BLOCK_TIMEOUT_MS_MAX)
}

/// Resolve the filter regex char-length cap (`SSH_FILTER_REGEX_MAX`).
#[must_use]
pub fn resolve_filter_regex_max() -> usize {
    env::var(FILTER_REGEX_MAX_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_FILTER_REGEX_MAX)
        .clamp(FILTER_REGEX_MAX_MIN, FILTER_REGEX_MAX_CAP)
}

/// Resolve the replay window byte cap (`SSH_REPLAY_WINDOW_BYTES`).
#[must_use]
pub fn resolve_replay_window_bytes() -> usize {
    env::var(REPLAY_WINDOW_BYTES_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_REPLAY_WINDOW_BYTES)
        .clamp(REPLAY_WINDOW_BYTES_MIN, REPLAY_WINDOW_BYTES_MAX)
}

// ---------------------------------------------------------------------------
// v5 Phase 4 — NDJSON daemon-specific resolvers
// ---------------------------------------------------------------------------
// These resolvers govern only the `ssh-mcp-tail` binary; the HTTP and
// stdio binaries never read them. Defaults match `docs/INSTRUCTIONS_DAEMON.md`
// and the limits table in ADR 0008.

/// Default cap on a single NDJSON line received on the daemon's stdin
/// (1 MiB).
pub const DEFAULT_NDJSON_LINE_MAX: usize = 1_048_576;
/// Floor for the NDJSON line cap. Below this realistic ops would be
/// rejected.
pub const NDJSON_LINE_MAX_MIN: usize = 1_024;
/// Hard cap on the NDJSON line cap (16 MiB).
pub const NDJSON_LINE_MAX_CAP: usize = 16 * 1_048_576;
/// Environment variable for the NDJSON line cap.
pub const NDJSON_LINE_MAX_ENV_VAR: &str = "SSH_NDJSON_LINE_MAX";

/// Default heartbeat emit interval (seconds).
pub const DEFAULT_HEARTBEAT_INTERVAL_S: u64 = 30;
/// Floor for the heartbeat interval.
pub const HEARTBEAT_INTERVAL_S_MIN: u64 = 1;
/// Cap for the heartbeat interval.
pub const HEARTBEAT_INTERVAL_S_MAX: u64 = 3_600;
/// Environment variable for the heartbeat interval.
pub const HEARTBEAT_INTERVAL_S_ENV_VAR: &str = "SSH_HEARTBEAT_INTERVAL_S";

/// Default daemon-stats emit interval (seconds).
pub const DEFAULT_DAEMON_STATS_INTERVAL_S: u64 = 60;
/// Floor for the daemon-stats interval.
pub const DAEMON_STATS_INTERVAL_S_MIN: u64 = 1;
/// Cap for the daemon-stats interval.
pub const DAEMON_STATS_INTERVAL_S_MAX: u64 = 3_600;
/// Environment variable for the daemon-stats interval.
pub const DAEMON_STATS_INTERVAL_S_ENV_VAR: &str = "SSH_DAEMON_STATS_INTERVAL_S";

/// Default hard-shutdown deadline (seconds) for the daemon's grace
/// drain.
pub const DEFAULT_GRACE_HARD_TIMEOUT_S: u64 = 30;
/// Floor for the hard-shutdown deadline.
pub const GRACE_HARD_TIMEOUT_S_MIN: u64 = 1;
/// Cap for the hard-shutdown deadline.
pub const GRACE_HARD_TIMEOUT_S_MAX: u64 = 3_600;
/// Environment variable for the hard-shutdown deadline.
pub const GRACE_HARD_TIMEOUT_S_ENV_VAR: &str = "SSH_GRACE_HARD_TIMEOUT_S";

/// Default for the optional pretty-print flag on outbound NDJSON.
pub const DEFAULT_NDJSON_PRETTY: bool = false;
/// Environment variable for the pretty-print flag.
pub const NDJSON_PRETTY_ENV_VAR: &str = "SSH_NDJSON_PRETTY";

/// Resolve the per-line NDJSON byte cap (`SSH_NDJSON_LINE_MAX`).
#[must_use]
pub fn resolve_ndjson_line_max() -> usize {
    env::var(NDJSON_LINE_MAX_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_NDJSON_LINE_MAX)
        .clamp(NDJSON_LINE_MAX_MIN, NDJSON_LINE_MAX_CAP)
}

/// Resolve the heartbeat emit interval (`SSH_HEARTBEAT_INTERVAL_S`).
#[must_use]
pub fn resolve_heartbeat_interval_s() -> u64 {
    env::var(HEARTBEAT_INTERVAL_S_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_HEARTBEAT_INTERVAL_S)
        .clamp(HEARTBEAT_INTERVAL_S_MIN, HEARTBEAT_INTERVAL_S_MAX)
}

/// Resolve the daemon-stats emit interval (`SSH_DAEMON_STATS_INTERVAL_S`).
#[must_use]
pub fn resolve_daemon_stats_interval_s() -> u64 {
    env::var(DAEMON_STATS_INTERVAL_S_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_DAEMON_STATS_INTERVAL_S)
        .clamp(DAEMON_STATS_INTERVAL_S_MIN, DAEMON_STATS_INTERVAL_S_MAX)
}

/// Resolve the hard-shutdown grace deadline (`SSH_GRACE_HARD_TIMEOUT_S`).
#[must_use]
pub fn resolve_grace_hard_timeout_s() -> u64 {
    env::var(GRACE_HARD_TIMEOUT_S_ENV_VAR)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_GRACE_HARD_TIMEOUT_S)
        .clamp(GRACE_HARD_TIMEOUT_S_MIN, GRACE_HARD_TIMEOUT_S_MAX)
}

/// Resolve the pretty-print flag for outbound NDJSON (`SSH_NDJSON_PRETTY`).
/// Defaults to `false`; any value other than `true` / `1` returns false.
#[must_use]
pub fn resolve_ndjson_pretty() -> bool {
    env::var(NDJSON_PRETTY_ENV_VAR)
        .ok()
        .map_or(DEFAULT_NDJSON_PRETTY, |v| {
            matches!(v.as_str(), "true" | "1" | "yes")
        })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test assertions and mutex locks may use unwrap"
)]
#[allow(
    unsafe_code,
    reason = "Rust 2024 requires unsafe for env::set_var; tests serialize via ENV_TEST_MUTEX"
)]
mod tests {
    use std::sync::{LazyLock, Mutex as StdMutex};

    use super::*;

    // Use a mutex to serialize env var tests to avoid race conditions
    // SAFETY: Tests are serialized via ENV_TEST_MUTEX to prevent data races
    static ENV_TEST_MUTEX: LazyLock<StdMutex<()>> = LazyLock::new(|| StdMutex::new(()));

    /// Helper to set an environment variable safely within tests.
    /// SAFETY: Must be called while holding ENV_TEST_MUTEX to prevent data races.
    unsafe fn set_env(key: &str, value: &str) {
        // SAFETY: Caller ensures ENV_TEST_MUTEX is held
        unsafe { env::set_var(key, value) };
    }

    /// Helper to remove an environment variable safely within tests.
    /// SAFETY: Must be called while holding ENV_TEST_MUTEX to prevent data races.
    unsafe fn remove_env(key: &str) {
        // SAFETY: Caller ensures ENV_TEST_MUTEX is held
        unsafe { env::remove_var(key) };
    }

    // --- v5 Phase 2 resolvers ---------------------------------------

    mod v5_phase_2 {
        use super::*;
        use crate::domain::subscription::LagPolicy;

        #[test]
        fn lane_buffer_defaults_to_1024() {
            let _g = ENV_TEST_MUTEX.lock().unwrap();
            unsafe { remove_env(LANE_BUFFER_ENV_VAR) };
            assert_eq!(resolve_lane_buffer(), DEFAULT_LANE_BUFFER);
        }

        #[test]
        fn lane_buffer_clamps_below_floor() {
            let _g = ENV_TEST_MUTEX.lock().unwrap();
            unsafe { set_env(LANE_BUFFER_ENV_VAR, "1") };
            assert_eq!(resolve_lane_buffer(), LANE_BUFFER_MIN);
            unsafe { remove_env(LANE_BUFFER_ENV_VAR) };
        }

        #[test]
        fn lane_buffer_clamps_above_cap() {
            let _g = ENV_TEST_MUTEX.lock().unwrap();
            unsafe { set_env(LANE_BUFFER_ENV_VAR, "9999999") };
            assert_eq!(resolve_lane_buffer(), LANE_BUFFER_MAX);
            unsafe { remove_env(LANE_BUFFER_ENV_VAR) };
        }

        #[test]
        fn mux_buffer_defaults_to_8192() {
            let _g = ENV_TEST_MUTEX.lock().unwrap();
            unsafe { remove_env(MUX_BUFFER_ENV_VAR) };
            assert_eq!(resolve_mux_buffer(), DEFAULT_MUX_BUFFER);
        }

        #[test]
        fn max_subs_per_uri_defaults_to_16() {
            let _g = ENV_TEST_MUTEX.lock().unwrap();
            unsafe { remove_env(MAX_SUBS_PER_URI_ENV_VAR) };
            assert_eq!(resolve_max_subs_per_uri(), DEFAULT_MAX_SUBS_PER_URI);
        }

        #[test]
        fn max_subs_per_uri_floors_at_one() {
            let _g = ENV_TEST_MUTEX.lock().unwrap();
            unsafe { set_env(MAX_SUBS_PER_URI_ENV_VAR, "0") };
            assert_eq!(resolve_max_subs_per_uri(), MAX_SUBS_PER_URI_MIN);
            unsafe { remove_env(MAX_SUBS_PER_URI_ENV_VAR) };
        }

        #[test]
        fn max_subs_total_defaults_to_1024() {
            let _g = ENV_TEST_MUTEX.lock().unwrap();
            unsafe { remove_env(MAX_SUBS_TOTAL_ENV_VAR) };
            assert_eq!(resolve_max_subs_total(), DEFAULT_MAX_SUBS_TOTAL);
        }

        #[test]
        fn max_subs_total_floors_at_one() {
            let _g = ENV_TEST_MUTEX.lock().unwrap();
            unsafe { set_env(MAX_SUBS_TOTAL_ENV_VAR, "0") };
            assert_eq!(resolve_max_subs_total(), MAX_SUBS_TOTAL_MIN);
            unsafe { remove_env(MAX_SUBS_TOTAL_ENV_VAR) };
        }

        #[test]
        fn lag_policy_default_is_snapshot() {
            let _g = ENV_TEST_MUTEX.lock().unwrap();
            unsafe { remove_env(LAG_POLICY_DEFAULT_ENV_VAR) };
            assert_eq!(resolve_lag_policy_default(), LagPolicy::Snapshot);
        }

        #[test]
        fn lag_policy_default_parses_block_slow() {
            let _g = ENV_TEST_MUTEX.lock().unwrap();
            unsafe { set_env(LAG_POLICY_DEFAULT_ENV_VAR, "block_slow") };
            assert_eq!(resolve_lag_policy_default(), LagPolicy::BlockSlow);
            unsafe { remove_env(LAG_POLICY_DEFAULT_ENV_VAR) };
        }

        #[test]
        fn lag_policy_default_parses_drop_oldest() {
            let _g = ENV_TEST_MUTEX.lock().unwrap();
            unsafe { set_env(LAG_POLICY_DEFAULT_ENV_VAR, "drop_oldest") };
            assert_eq!(resolve_lag_policy_default(), LagPolicy::DropOldest);
            unsafe { remove_env(LAG_POLICY_DEFAULT_ENV_VAR) };
        }

        #[test]
        fn lag_policy_default_parses_drop_newest() {
            let _g = ENV_TEST_MUTEX.lock().unwrap();
            unsafe { set_env(LAG_POLICY_DEFAULT_ENV_VAR, "drop_newest") };
            assert_eq!(resolve_lag_policy_default(), LagPolicy::DropNewest);
            unsafe { remove_env(LAG_POLICY_DEFAULT_ENV_VAR) };
        }

        #[test]
        fn lag_policy_default_falls_back_on_unknown() {
            let _g = ENV_TEST_MUTEX.lock().unwrap();
            unsafe { set_env(LAG_POLICY_DEFAULT_ENV_VAR, "BlockSlow") };
            // Wire form is snake_case — UpperCamelCase parses as None.
            assert_eq!(resolve_lag_policy_default(), LagPolicy::Snapshot);
            unsafe { remove_env(LAG_POLICY_DEFAULT_ENV_VAR) };
        }

        #[test]
        fn bp_block_timeout_defaults_to_5000() {
            let _g = ENV_TEST_MUTEX.lock().unwrap();
            unsafe { remove_env(BP_BLOCK_TIMEOUT_MS_ENV_VAR) };
            assert_eq!(resolve_bp_block_timeout_ms(), DEFAULT_BP_BLOCK_TIMEOUT_MS);
        }

        #[test]
        fn bp_block_timeout_clamps_to_floor() {
            let _g = ENV_TEST_MUTEX.lock().unwrap();
            unsafe { set_env(BP_BLOCK_TIMEOUT_MS_ENV_VAR, "1") };
            assert_eq!(resolve_bp_block_timeout_ms(), BP_BLOCK_TIMEOUT_MS_MIN);
            unsafe { remove_env(BP_BLOCK_TIMEOUT_MS_ENV_VAR) };
        }

        #[test]
        fn filter_regex_max_defaults_to_1024() {
            let _g = ENV_TEST_MUTEX.lock().unwrap();
            unsafe { remove_env(FILTER_REGEX_MAX_ENV_VAR) };
            assert_eq!(resolve_filter_regex_max(), DEFAULT_FILTER_REGEX_MAX);
        }

        #[test]
        fn replay_window_bytes_defaults_to_1mib() {
            let _g = ENV_TEST_MUTEX.lock().unwrap();
            unsafe { remove_env(REPLAY_WINDOW_BYTES_ENV_VAR) };
            assert_eq!(resolve_replay_window_bytes(), DEFAULT_REPLAY_WINDOW_BYTES);
        }

        #[test]
        fn replay_window_bytes_clamps_to_floor() {
            let _g = ENV_TEST_MUTEX.lock().unwrap();
            unsafe { set_env(REPLAY_WINDOW_BYTES_ENV_VAR, "1") };
            assert_eq!(resolve_replay_window_bytes(), REPLAY_WINDOW_BYTES_MIN);
            unsafe { remove_env(REPLAY_WINDOW_BYTES_ENV_VAR) };
        }
    }

    mod config_resolution {
        use super::*;

        mod connect_timeout {
            use super::*;

            #[test]
            fn test_uses_param_when_provided() {
                let result = resolve_connect_timeout(Some(60));
                assert_eq!(result, Duration::from_secs(60));
            }

            #[test]
            fn test_param_takes_priority_over_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(CONNECT_TIMEOUT_ENV_VAR, "120");
                }
                let result = resolve_connect_timeout(Some(45));
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(CONNECT_TIMEOUT_ENV_VAR);
                }
                assert_eq!(result, Duration::from_secs(45));
            }

            #[test]
            fn test_uses_env_var_when_no_param() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(CONNECT_TIMEOUT_ENV_VAR, "90");
                }
                let result = resolve_connect_timeout(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(CONNECT_TIMEOUT_ENV_VAR);
                }
                assert_eq!(result, Duration::from_secs(90));
            }

            #[test]
            fn test_uses_default_when_no_param_or_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(CONNECT_TIMEOUT_ENV_VAR);
                }
                let result = resolve_connect_timeout(None);
                assert_eq!(result, DEFAULT_CONNECT_TIMEOUT);
            }

            #[test]
            fn test_ignores_invalid_env_var() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(CONNECT_TIMEOUT_ENV_VAR, "invalid");
                }
                let result = resolve_connect_timeout(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(CONNECT_TIMEOUT_ENV_VAR);
                }
                assert_eq!(result, DEFAULT_CONNECT_TIMEOUT);
            }

            #[test]
            fn test_ignores_negative_env_var() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(CONNECT_TIMEOUT_ENV_VAR, "-10");
                }
                let result = resolve_connect_timeout(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(CONNECT_TIMEOUT_ENV_VAR);
                }
                // Parsing fails for negative u64, so default is used
                assert_eq!(result, DEFAULT_CONNECT_TIMEOUT);
            }
        }

        mod command_timeout {
            use super::*;

            #[test]
            fn test_uses_param_when_provided() {
                let result = resolve_command_timeout(Some(120));
                assert_eq!(result, Duration::from_secs(120));
            }

            #[test]
            fn test_param_takes_priority_over_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(COMMAND_TIMEOUT_ENV_VAR, "300");
                }
                let result = resolve_command_timeout(Some(60));
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMMAND_TIMEOUT_ENV_VAR);
                }
                assert_eq!(result, Duration::from_secs(60));
            }

            #[test]
            fn test_uses_env_var_when_no_param() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(COMMAND_TIMEOUT_ENV_VAR, "240");
                }
                let result = resolve_command_timeout(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMMAND_TIMEOUT_ENV_VAR);
                }
                assert_eq!(result, Duration::from_secs(240));
            }

            #[test]
            fn test_uses_default_when_no_param_or_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMMAND_TIMEOUT_ENV_VAR);
                }
                let result = resolve_command_timeout(None);
                assert_eq!(result, DEFAULT_COMMAND_TIMEOUT);
            }

            #[test]
            fn test_ignores_invalid_env_var() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(COMMAND_TIMEOUT_ENV_VAR, "not_a_number");
                }
                let result = resolve_command_timeout(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMMAND_TIMEOUT_ENV_VAR);
                }
                assert_eq!(result, DEFAULT_COMMAND_TIMEOUT);
            }
        }

        mod max_retries {
            use super::*;

            #[test]
            fn test_uses_param_when_provided() {
                let result = resolve_max_retries(Some(5));
                assert_eq!(result, 5);
            }

            #[test]
            fn test_param_takes_priority_over_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(MAX_RETRIES_ENV_VAR, "10");
                }
                let result = resolve_max_retries(Some(2));
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(MAX_RETRIES_ENV_VAR);
                }
                assert_eq!(result, 2);
            }

            #[test]
            fn test_uses_env_var_when_no_param() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(MAX_RETRIES_ENV_VAR, "7");
                }
                let result = resolve_max_retries(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(MAX_RETRIES_ENV_VAR);
                }
                assert_eq!(result, 7);
            }

            #[test]
            fn test_uses_default_when_no_param_or_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(MAX_RETRIES_ENV_VAR);
                }
                let result = resolve_max_retries(None);
                assert_eq!(result, DEFAULT_MAX_RETRIES);
            }

            #[test]
            fn test_zero_retries_is_valid() {
                let result = resolve_max_retries(Some(0));
                assert_eq!(result, 0);
            }

            #[test]
            fn test_ignores_invalid_env_var() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(MAX_RETRIES_ENV_VAR, "abc");
                }
                let result = resolve_max_retries(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(MAX_RETRIES_ENV_VAR);
                }
                assert_eq!(result, DEFAULT_MAX_RETRIES);
            }
        }

        mod retry_delay {
            use super::*;

            #[test]
            fn test_uses_param_when_provided() {
                let result = resolve_retry_delay(Some(2000));
                assert_eq!(result, Duration::from_millis(2000));
            }

            #[test]
            fn test_param_takes_priority_over_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(RETRY_DELAY_MS_ENV_VAR, "5000");
                }
                let result = resolve_retry_delay(Some(500));
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(RETRY_DELAY_MS_ENV_VAR);
                }
                assert_eq!(result, Duration::from_millis(500));
            }

            #[test]
            fn test_uses_env_var_when_no_param() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(RETRY_DELAY_MS_ENV_VAR, "3000");
                }
                let result = resolve_retry_delay(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(RETRY_DELAY_MS_ENV_VAR);
                }
                assert_eq!(result, Duration::from_millis(3000));
            }

            #[test]
            fn test_uses_default_when_no_param_or_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(RETRY_DELAY_MS_ENV_VAR);
                }
                let result = resolve_retry_delay(None);
                assert_eq!(result, DEFAULT_RETRY_DELAY);
            }

            #[test]
            fn test_ignores_invalid_env_var() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(RETRY_DELAY_MS_ENV_VAR, "xyz");
                }
                let result = resolve_retry_delay(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(RETRY_DELAY_MS_ENV_VAR);
                }
                assert_eq!(result, DEFAULT_RETRY_DELAY);
            }
        }

        mod inactivity_timeout {
            use super::*;

            #[test]
            fn test_uses_default_when_no_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(INACTIVITY_TIMEOUT_ENV_VAR);
                }
                let result = resolve_inactivity_timeout();
                assert_eq!(result, DEFAULT_INACTIVITY_TIMEOUT);
            }

            #[test]
            fn test_default_is_300_seconds() {
                assert_eq!(DEFAULT_INACTIVITY_TIMEOUT, Duration::from_secs(300));
            }

            #[test]
            fn test_uses_env_var() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(INACTIVITY_TIMEOUT_ENV_VAR, "600");
                }
                let result = resolve_inactivity_timeout();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(INACTIVITY_TIMEOUT_ENV_VAR);
                }
                assert_eq!(result, Duration::from_secs(600));
            }

            #[test]
            fn test_ignores_invalid_env_var() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(INACTIVITY_TIMEOUT_ENV_VAR, "invalid");
                }
                let result = resolve_inactivity_timeout();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(INACTIVITY_TIMEOUT_ENV_VAR);
                }
                assert_eq!(result, DEFAULT_INACTIVITY_TIMEOUT);
            }
        }

        mod compression {
            use super::*;

            #[test]
            fn test_uses_param_true_when_provided() {
                let result = resolve_compression(Some(true));
                assert!(result);
            }

            #[test]
            fn test_uses_param_false_when_provided() {
                let result = resolve_compression(Some(false));
                assert!(!result);
            }

            #[test]
            fn test_param_takes_priority_over_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(COMPRESSION_ENV_VAR, "true");
                }
                let result = resolve_compression(Some(false));
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMPRESSION_ENV_VAR);
                }
                assert!(!result);
            }

            #[test]
            fn test_env_var_true_lowercase() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(COMPRESSION_ENV_VAR, "true");
                }
                let result = resolve_compression(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMPRESSION_ENV_VAR);
                }
                assert!(result);
            }

            #[test]
            fn test_env_var_true_uppercase() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(COMPRESSION_ENV_VAR, "TRUE");
                }
                let result = resolve_compression(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMPRESSION_ENV_VAR);
                }
                assert!(result);
            }

            #[test]
            fn test_env_var_true_mixed_case() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(COMPRESSION_ENV_VAR, "TrUe");
                }
                let result = resolve_compression(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMPRESSION_ENV_VAR);
                }
                assert!(result);
            }

            #[test]
            fn test_env_var_one() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(COMPRESSION_ENV_VAR, "1");
                }
                let result = resolve_compression(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMPRESSION_ENV_VAR);
                }
                assert!(result);
            }

            #[test]
            fn test_env_var_false_lowercase() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(COMPRESSION_ENV_VAR, "false");
                }
                let result = resolve_compression(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMPRESSION_ENV_VAR);
                }
                assert!(!result);
            }

            #[test]
            fn test_env_var_zero() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(COMPRESSION_ENV_VAR, "0");
                }
                let result = resolve_compression(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMPRESSION_ENV_VAR);
                }
                assert!(!result);
            }

            #[test]
            fn test_env_var_random_value_is_false() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(COMPRESSION_ENV_VAR, "yes");
                }
                let result = resolve_compression(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMPRESSION_ENV_VAR);
                }
                // "yes" is not "true" or "1", so it's false
                assert!(!result);
            }

            #[test]
            fn test_default_is_true() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMPRESSION_ENV_VAR);
                }
                let result = resolve_compression(None);
                assert!(result);
            }
        }

        mod parse_byte_size_tests {
            use super::*;

            #[test]
            fn test_plain_number_is_bytes() {
                assert_eq!(parse_byte_size("1024"), Some(1024));
            }

            #[test]
            fn test_bytes_suffix() {
                assert_eq!(parse_byte_size("100b"), Some(100));
            }

            #[test]
            fn test_kilobytes() {
                assert_eq!(parse_byte_size("1k"), Some(1024));
                assert_eq!(parse_byte_size("1kb"), Some(1024));
            }

            #[test]
            fn test_megabytes() {
                assert_eq!(parse_byte_size("10m"), Some(10 * 1024 * 1024));
                assert_eq!(parse_byte_size("10mb"), Some(10 * 1024 * 1024));
            }

            #[test]
            fn test_gigabytes() {
                assert_eq!(parse_byte_size("1g"), Some(1024 * 1024 * 1024));
                assert_eq!(parse_byte_size("2gb"), Some(2 * 1024 * 1024 * 1024));
            }

            #[test]
            fn test_terabytes() {
                assert_eq!(parse_byte_size("1t"), Some(1024_u64 * 1024 * 1024 * 1024));
                assert_eq!(parse_byte_size("1tb"), Some(1024_u64 * 1024 * 1024 * 1024));
            }

            #[test]
            fn test_case_insensitive() {
                assert_eq!(parse_byte_size("10M"), Some(10 * 1024 * 1024));
                assert_eq!(parse_byte_size("1GB"), Some(1024 * 1024 * 1024));
                assert_eq!(parse_byte_size("512K"), Some(512 * 1024));
            }

            #[test]
            fn test_whitespace_trimmed() {
                assert_eq!(parse_byte_size("  10m  "), Some(10 * 1024 * 1024));
            }

            #[test]
            fn test_empty_is_none() {
                assert_eq!(parse_byte_size(""), None);
                assert_eq!(parse_byte_size("  "), None);
            }

            #[test]
            fn test_invalid_is_none() {
                assert_eq!(parse_byte_size("abc"), None);
                assert_eq!(parse_byte_size("m"), None);
            }

            #[test]
            fn test_zero_is_valid() {
                assert_eq!(parse_byte_size("0"), Some(0));
                assert_eq!(parse_byte_size("0m"), Some(0));
            }
        }

        mod shell_inactivity_ttl {
            use super::*;

            #[test]
            fn test_uses_param_when_provided() {
                let result = resolve_shell_inactivity_ttl(Some(120));
                assert_eq!(result, Duration::from_secs(120));
            }

            #[test]
            fn test_param_takes_priority_over_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(SHELL_INACTIVITY_TTL_ENV_VAR, "300") };
                let result = resolve_shell_inactivity_ttl(Some(60));
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(SHELL_INACTIVITY_TTL_ENV_VAR) };
                assert_eq!(result, Duration::from_secs(60));
            }

            #[test]
            fn test_uses_env_var_when_no_param() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(SHELL_INACTIVITY_TTL_ENV_VAR, "900") };
                let result = resolve_shell_inactivity_ttl(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(SHELL_INACTIVITY_TTL_ENV_VAR) };
                assert_eq!(result, Duration::from_secs(900));
            }

            #[test]
            fn test_uses_default_when_no_param_or_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(SHELL_INACTIVITY_TTL_ENV_VAR) };
                let result = resolve_shell_inactivity_ttl(None);
                assert_eq!(result, DEFAULT_SHELL_INACTIVITY_TTL);
            }

            #[test]
            fn test_default_is_600_seconds() {
                assert_eq!(DEFAULT_SHELL_INACTIVITY_TTL, Duration::from_secs(600));
            }

            #[test]
            fn test_ignores_invalid_env_var() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(SHELL_INACTIVITY_TTL_ENV_VAR, "invalid") };
                let result = resolve_shell_inactivity_ttl(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(SHELL_INACTIVITY_TTL_ENV_VAR) };
                assert_eq!(result, DEFAULT_SHELL_INACTIVITY_TTL);
            }
        }

        mod shell_max_buffer_size {
            use super::*;

            #[test]
            fn test_uses_param_when_provided() {
                let result = resolve_shell_max_buffer_size(Some("512k"));
                assert_eq!(result, 512 * 1024);
            }

            #[test]
            fn test_param_takes_priority_over_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(SHELL_MAX_BUFFER_SIZE_ENV_VAR, "1g") };
                let result = resolve_shell_max_buffer_size(Some("5m"));
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(SHELL_MAX_BUFFER_SIZE_ENV_VAR) };
                assert_eq!(result, 5 * 1024 * 1024);
            }

            #[test]
            fn test_uses_env_var_when_no_param() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(SHELL_MAX_BUFFER_SIZE_ENV_VAR, "20m") };
                let result = resolve_shell_max_buffer_size(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(SHELL_MAX_BUFFER_SIZE_ENV_VAR) };
                assert_eq!(result, 20 * 1024 * 1024);
            }

            #[test]
            fn test_uses_default_when_no_param_or_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(SHELL_MAX_BUFFER_SIZE_ENV_VAR) };
                let result = resolve_shell_max_buffer_size(None);
                assert_eq!(result, DEFAULT_SHELL_MAX_BUFFER_SIZE);
            }

            #[test]
            fn test_default_is_10mb() {
                assert_eq!(DEFAULT_SHELL_MAX_BUFFER_SIZE, 10 * 1024 * 1024);
            }

            #[test]
            fn test_ignores_invalid_param() {
                let result = resolve_shell_max_buffer_size(Some("invalid"));
                assert_eq!(result, DEFAULT_SHELL_MAX_BUFFER_SIZE);
            }

            #[test]
            fn test_ignores_invalid_env_var() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(SHELL_MAX_BUFFER_SIZE_ENV_VAR, "invalid") };
                let result = resolve_shell_max_buffer_size(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(SHELL_MAX_BUFFER_SIZE_ENV_VAR) };
                assert_eq!(result, DEFAULT_SHELL_MAX_BUFFER_SIZE);
            }
        }

        mod command_broadcast_cap {
            use super::*;

            #[test]
            fn test_default_when_env_unset() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(COMMAND_BROADCAST_CAP_ENV_VAR) };
                let result = resolve_command_broadcast_cap();
                assert_eq!(result, DEFAULT_COMMAND_BROADCAST_CAP);
            }

            #[test]
            fn test_uses_env_var_when_in_range() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(COMMAND_BROADCAST_CAP_ENV_VAR, "2048") };
                let result = resolve_command_broadcast_cap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(COMMAND_BROADCAST_CAP_ENV_VAR) };
                assert_eq!(result, 2048);
            }

            #[test]
            fn test_clamps_to_floor() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(COMMAND_BROADCAST_CAP_ENV_VAR, "1") };
                let result = resolve_command_broadcast_cap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(COMMAND_BROADCAST_CAP_ENV_VAR) };
                assert_eq!(result, COMMAND_BROADCAST_CAP_MIN);
            }

            #[test]
            fn test_clamps_to_cap() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(COMMAND_BROADCAST_CAP_ENV_VAR, "9999999") };
                let result = resolve_command_broadcast_cap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(COMMAND_BROADCAST_CAP_ENV_VAR) };
                assert_eq!(result, COMMAND_BROADCAST_CAP_MAX);
            }

            #[test]
            fn test_ignores_invalid_env_var() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(COMMAND_BROADCAST_CAP_ENV_VAR, "not-a-number") };
                let result = resolve_command_broadcast_cap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(COMMAND_BROADCAST_CAP_ENV_VAR) };
                assert_eq!(result, DEFAULT_COMMAND_BROADCAST_CAP);
            }

            #[test]
            fn test_constants_are_consistent() {
                assert!(COMMAND_BROADCAST_CAP_MIN <= DEFAULT_COMMAND_BROADCAST_CAP);
                assert!(DEFAULT_COMMAND_BROADCAST_CAP <= COMMAND_BROADCAST_CAP_MAX);
                assert_eq!(DEFAULT_COMMAND_BROADCAST_CAP, 1024);
                assert_eq!(COMMAND_BROADCAST_CAP_MAX, 65_536);
                assert_eq!(COMMAND_BROADCAST_CAP_MIN, 16);
            }
        }

        mod shell_broadcast_cap {
            use super::*;

            #[test]
            fn test_default_when_env_unset() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(SHELL_BROADCAST_CAP_ENV_VAR) };
                let result = resolve_shell_broadcast_cap();
                assert_eq!(result, DEFAULT_SHELL_BROADCAST_CAP);
            }

            #[test]
            fn test_uses_env_var_when_in_range() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(SHELL_BROADCAST_CAP_ENV_VAR, "2048") };
                let result = resolve_shell_broadcast_cap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(SHELL_BROADCAST_CAP_ENV_VAR) };
                assert_eq!(result, 2048);
            }

            #[test]
            fn test_clamps_to_floor() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(SHELL_BROADCAST_CAP_ENV_VAR, "1") };
                let result = resolve_shell_broadcast_cap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(SHELL_BROADCAST_CAP_ENV_VAR) };
                assert_eq!(result, SHELL_BROADCAST_CAP_MIN);
            }

            #[test]
            fn test_clamps_to_cap() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(SHELL_BROADCAST_CAP_ENV_VAR, "9999999") };
                let result = resolve_shell_broadcast_cap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(SHELL_BROADCAST_CAP_ENV_VAR) };
                assert_eq!(result, SHELL_BROADCAST_CAP_MAX);
            }

            #[test]
            fn test_ignores_invalid_env_var() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(SHELL_BROADCAST_CAP_ENV_VAR, "not-a-number") };
                let result = resolve_shell_broadcast_cap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(SHELL_BROADCAST_CAP_ENV_VAR) };
                assert_eq!(result, DEFAULT_SHELL_BROADCAST_CAP);
            }

            #[test]
            fn test_constants_are_consistent() {
                assert!(SHELL_BROADCAST_CAP_MIN <= DEFAULT_SHELL_BROADCAST_CAP);
                assert!(DEFAULT_SHELL_BROADCAST_CAP <= SHELL_BROADCAST_CAP_MAX);
                assert_eq!(DEFAULT_SHELL_BROADCAST_CAP, 1024);
                assert_eq!(SHELL_BROADCAST_CAP_MAX, 65_536);
                assert_eq!(SHELL_BROADCAST_CAP_MIN, 16);
            }
        }

        mod transfer_broadcast_cap {
            use super::*;

            #[test]
            fn test_default_when_env_unset() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(TRANSFER_BROADCAST_CAP_ENV_VAR) };
                let result = resolve_transfer_broadcast_cap();
                assert_eq!(result, DEFAULT_TRANSFER_BROADCAST_CAP);
            }

            #[test]
            fn test_uses_env_var_when_in_range() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(TRANSFER_BROADCAST_CAP_ENV_VAR, "512") };
                let result = resolve_transfer_broadcast_cap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(TRANSFER_BROADCAST_CAP_ENV_VAR) };
                assert_eq!(result, 512);
            }

            #[test]
            fn test_clamps_to_floor() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(TRANSFER_BROADCAST_CAP_ENV_VAR, "1") };
                let result = resolve_transfer_broadcast_cap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(TRANSFER_BROADCAST_CAP_ENV_VAR) };
                assert_eq!(result, TRANSFER_BROADCAST_CAP_MIN);
            }

            #[test]
            fn test_clamps_to_cap() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(TRANSFER_BROADCAST_CAP_ENV_VAR, "9999999") };
                let result = resolve_transfer_broadcast_cap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(TRANSFER_BROADCAST_CAP_ENV_VAR) };
                assert_eq!(result, TRANSFER_BROADCAST_CAP_MAX);
            }

            #[test]
            fn test_ignores_invalid_env_var() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(TRANSFER_BROADCAST_CAP_ENV_VAR, "not-a-number") };
                let result = resolve_transfer_broadcast_cap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(TRANSFER_BROADCAST_CAP_ENV_VAR) };
                assert_eq!(result, DEFAULT_TRANSFER_BROADCAST_CAP);
            }

            #[test]
            fn test_constants_are_consistent() {
                assert!(TRANSFER_BROADCAST_CAP_MIN <= DEFAULT_TRANSFER_BROADCAST_CAP);
                assert!(DEFAULT_TRANSFER_BROADCAST_CAP <= TRANSFER_BROADCAST_CAP_MAX);
                assert_eq!(DEFAULT_TRANSFER_BROADCAST_CAP, 256);
                assert_eq!(TRANSFER_BROADCAST_CAP_MAX, 4096);
                assert_eq!(TRANSFER_BROADCAST_CAP_MIN, 8);
            }
        }

        mod session_broadcast_cap {
            use super::*;

            #[test]
            fn test_default_when_env_unset() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(SESSION_BROADCAST_CAP_ENV_VAR) };
                let result = resolve_session_broadcast_cap();
                assert_eq!(result, DEFAULT_SESSION_BROADCAST_CAP);
            }

            #[test]
            fn test_uses_env_var_when_in_range() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(SESSION_BROADCAST_CAP_ENV_VAR, "1024") };
                let result = resolve_session_broadcast_cap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(SESSION_BROADCAST_CAP_ENV_VAR) };
                assert_eq!(result, 1024);
            }

            #[test]
            fn test_clamps_to_floor() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(SESSION_BROADCAST_CAP_ENV_VAR, "1") };
                let result = resolve_session_broadcast_cap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(SESSION_BROADCAST_CAP_ENV_VAR) };
                assert_eq!(result, SESSION_BROADCAST_CAP_MIN);
            }

            #[test]
            fn test_clamps_to_cap() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(SESSION_BROADCAST_CAP_ENV_VAR, "9999999") };
                let result = resolve_session_broadcast_cap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(SESSION_BROADCAST_CAP_ENV_VAR) };
                assert_eq!(result, SESSION_BROADCAST_CAP_MAX);
            }

            #[test]
            fn test_ignores_invalid_env_var() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(SESSION_BROADCAST_CAP_ENV_VAR, "not-a-number") };
                let result = resolve_session_broadcast_cap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(SESSION_BROADCAST_CAP_ENV_VAR) };
                assert_eq!(result, DEFAULT_SESSION_BROADCAST_CAP);
            }

            #[test]
            fn test_constants_are_consistent() {
                assert!(SESSION_BROADCAST_CAP_MIN <= DEFAULT_SESSION_BROADCAST_CAP);
                assert!(DEFAULT_SESSION_BROADCAST_CAP <= SESSION_BROADCAST_CAP_MAX);
                assert_eq!(DEFAULT_SESSION_BROADCAST_CAP, 256);
                assert_eq!(SESSION_BROADCAST_CAP_MAX, 4096);
                assert_eq!(SESSION_BROADCAST_CAP_MIN, 8);
            }
        }

        #[cfg(feature = "port_forward")]
        mod forward_broadcast_cap {
            use super::*;

            #[test]
            fn test_default_when_env_unset() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(FORWARD_BROADCAST_CAP_ENV_VAR) };
                let result = resolve_forward_broadcast_cap();
                assert_eq!(result, DEFAULT_FORWARD_BROADCAST_CAP);
            }

            #[test]
            fn test_uses_env_var_when_in_range() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(FORWARD_BROADCAST_CAP_ENV_VAR, "1024") };
                let result = resolve_forward_broadcast_cap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(FORWARD_BROADCAST_CAP_ENV_VAR) };
                assert_eq!(result, 1024);
            }

            #[test]
            fn test_clamps_to_floor() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(FORWARD_BROADCAST_CAP_ENV_VAR, "1") };
                let result = resolve_forward_broadcast_cap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(FORWARD_BROADCAST_CAP_ENV_VAR) };
                assert_eq!(result, FORWARD_BROADCAST_CAP_MIN);
            }

            #[test]
            fn test_clamps_to_cap() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(FORWARD_BROADCAST_CAP_ENV_VAR, "9999999") };
                let result = resolve_forward_broadcast_cap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(FORWARD_BROADCAST_CAP_ENV_VAR) };
                assert_eq!(result, FORWARD_BROADCAST_CAP_MAX);
            }

            #[test]
            fn test_ignores_invalid_env_var() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(FORWARD_BROADCAST_CAP_ENV_VAR, "not-a-number") };
                let result = resolve_forward_broadcast_cap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(FORWARD_BROADCAST_CAP_ENV_VAR) };
                assert_eq!(result, DEFAULT_FORWARD_BROADCAST_CAP);
            }

            #[test]
            fn test_constants_are_consistent() {
                assert!(FORWARD_BROADCAST_CAP_MIN <= DEFAULT_FORWARD_BROADCAST_CAP);
                assert!(DEFAULT_FORWARD_BROADCAST_CAP <= FORWARD_BROADCAST_CAP_MAX);
                assert_eq!(DEFAULT_FORWARD_BROADCAST_CAP, 256);
                assert_eq!(FORWARD_BROADCAST_CAP_MAX, 4096);
                assert_eq!(FORWARD_BROADCAST_CAP_MIN, 8);
            }
        }

        mod notify_debounce_ms {
            use super::*;

            #[test]
            fn test_default_when_env_unset() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(NOTIFY_DEBOUNCE_MS_ENV_VAR) };
                let result = resolve_notify_debounce_ms();
                assert_eq!(result, DEFAULT_NOTIFY_DEBOUNCE_MS);
            }

            #[test]
            fn test_uses_env_var_when_in_range() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(NOTIFY_DEBOUNCE_MS_ENV_VAR, "200") };
                let result = resolve_notify_debounce_ms();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(NOTIFY_DEBOUNCE_MS_ENV_VAR) };
                assert_eq!(result, 200);
            }

            #[test]
            fn test_clamps_to_floor() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(NOTIFY_DEBOUNCE_MS_ENV_VAR, "0") };
                let result = resolve_notify_debounce_ms();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(NOTIFY_DEBOUNCE_MS_ENV_VAR) };
                assert_eq!(result, NOTIFY_DEBOUNCE_MS_MIN);
            }

            #[test]
            fn test_clamps_to_cap() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(NOTIFY_DEBOUNCE_MS_ENV_VAR, "999999") };
                let result = resolve_notify_debounce_ms();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(NOTIFY_DEBOUNCE_MS_ENV_VAR) };
                assert_eq!(result, NOTIFY_DEBOUNCE_MS_MAX);
            }

            #[test]
            fn test_ignores_invalid_env_var() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(NOTIFY_DEBOUNCE_MS_ENV_VAR, "abc") };
                let result = resolve_notify_debounce_ms();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(NOTIFY_DEBOUNCE_MS_ENV_VAR) };
                assert_eq!(result, DEFAULT_NOTIFY_DEBOUNCE_MS);
            }
        }

        mod notify_force_flush_ms {
            use super::*;

            #[test]
            fn test_default_when_env_unset() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(NOTIFY_FORCE_FLUSH_MS_ENV_VAR) };
                let result = resolve_notify_force_flush_ms();
                assert_eq!(result, DEFAULT_NOTIFY_FORCE_FLUSH_MS);
            }

            #[test]
            fn test_clamps_to_floor() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(NOTIFY_FORCE_FLUSH_MS_ENV_VAR, "10") };
                let result = resolve_notify_force_flush_ms();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(NOTIFY_FORCE_FLUSH_MS_ENV_VAR) };
                assert_eq!(result, NOTIFY_FORCE_FLUSH_MS_MIN);
            }

            #[test]
            fn test_clamps_to_cap() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(NOTIFY_FORCE_FLUSH_MS_ENV_VAR, "9999999") };
                let result = resolve_notify_force_flush_ms();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(NOTIFY_FORCE_FLUSH_MS_ENV_VAR) };
                assert_eq!(result, NOTIFY_FORCE_FLUSH_MS_MAX);
            }
        }

        mod notify_keepalive_s {
            use super::*;

            #[test]
            fn test_default_when_env_unset() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(NOTIFY_KEEPALIVE_S_ENV_VAR) };
                let result = resolve_notify_keepalive_s();
                assert_eq!(result, DEFAULT_NOTIFY_KEEPALIVE_S);
            }

            #[test]
            fn test_clamps_to_floor() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(NOTIFY_KEEPALIVE_S_ENV_VAR, "1") };
                let result = resolve_notify_keepalive_s();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(NOTIFY_KEEPALIVE_S_ENV_VAR) };
                assert_eq!(result, NOTIFY_KEEPALIVE_S_MIN);
            }

            #[test]
            fn test_clamps_to_cap() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(NOTIFY_KEEPALIVE_S_ENV_VAR, "999999") };
                let result = resolve_notify_keepalive_s();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(NOTIFY_KEEPALIVE_S_ENV_VAR) };
                assert_eq!(result, NOTIFY_KEEPALIVE_S_MAX);
            }
        }

        mod peer_gc_interval_s {
            use super::*;

            #[test]
            fn test_default_when_env_unset() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(PEER_GC_INTERVAL_S_ENV_VAR) };
                let result = resolve_peer_gc_interval_s();
                assert_eq!(result, DEFAULT_PEER_GC_INTERVAL_S);
            }

            #[test]
            fn test_uses_env_when_in_range() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(PEER_GC_INTERVAL_S_ENV_VAR, "120") };
                let result = resolve_peer_gc_interval_s();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(PEER_GC_INTERVAL_S_ENV_VAR) };
                assert_eq!(result, 120);
            }

            #[test]
            fn test_clamps_to_floor() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(PEER_GC_INTERVAL_S_ENV_VAR, "1") };
                let result = resolve_peer_gc_interval_s();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(PEER_GC_INTERVAL_S_ENV_VAR) };
                assert_eq!(result, PEER_GC_INTERVAL_S_MIN);
            }

            #[test]
            fn test_clamps_to_cap() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(PEER_GC_INTERVAL_S_ENV_VAR, "999999") };
                let result = resolve_peer_gc_interval_s();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(PEER_GC_INTERVAL_S_ENV_VAR) };
                assert_eq!(result, PEER_GC_INTERVAL_S_MAX);
            }

            #[test]
            fn test_invalid_value_falls_back_to_default() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { set_env(PEER_GC_INTERVAL_S_ENV_VAR, "not-a-number") };
                let result = resolve_peer_gc_interval_s();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe { remove_env(PEER_GC_INTERVAL_S_ENV_VAR) };
                assert_eq!(result, DEFAULT_PEER_GC_INTERVAL_S);
            }
        }
    }
}
