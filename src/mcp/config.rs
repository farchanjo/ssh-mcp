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
//! | `SSH_COMPRESSION` | true | Enable zlib compression |

use std::env;

/// Default SSH connection timeout in seconds
pub(crate) const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 30;

/// Default SSH command execution timeout in seconds
pub(crate) const DEFAULT_COMMAND_TIMEOUT_SECS: u64 = 180;

/// Default maximum retry attempts for SSH connection
pub(crate) const DEFAULT_MAX_RETRIES: u32 = 3;

/// Default retry delay in milliseconds
pub(crate) const DEFAULT_RETRY_DELAY_MS: u64 = 1000;

/// Maximum retry delay cap in seconds (10 seconds)
pub(crate) const MAX_RETRY_DELAY_SECS: u64 = 10;

/// Environment variable name for SSH connection timeout
pub(crate) const CONNECT_TIMEOUT_ENV_VAR: &str = "SSH_CONNECT_TIMEOUT";

/// Environment variable name for SSH command execution timeout
pub(crate) const COMMAND_TIMEOUT_ENV_VAR: &str = "SSH_COMMAND_TIMEOUT";

/// Environment variable name for SSH max retries
pub(crate) const MAX_RETRIES_ENV_VAR: &str = "SSH_MAX_RETRIES";

/// Environment variable name for SSH retry delay in milliseconds
pub(crate) const RETRY_DELAY_MS_ENV_VAR: &str = "SSH_RETRY_DELAY_MS";

/// Environment variable name for SSH compression
pub(crate) const COMPRESSION_ENV_VAR: &str = "SSH_COMPRESSION";

/// Resolve the connection timeout value with priority: parameter -> env var -> default
pub(crate) fn resolve_connect_timeout(timeout_param: Option<u64>) -> u64 {
    // Priority 1: Use parameter if provided
    if let Some(timeout) = timeout_param {
        return timeout;
    }

    // Priority 2: Use environment variable if set
    if let Ok(env_timeout) = env::var(CONNECT_TIMEOUT_ENV_VAR)
        && let Ok(timeout) = env_timeout.parse::<u64>()
    {
        return timeout;
    }

    // Priority 3: Default value
    DEFAULT_CONNECT_TIMEOUT_SECS
}

/// Resolve the command execution timeout value with priority: parameter -> env var -> default
pub(crate) fn resolve_command_timeout(timeout_param: Option<u64>) -> u64 {
    // Priority 1: Use parameter if provided
    if let Some(timeout) = timeout_param {
        return timeout;
    }

    // Priority 2: Use environment variable if set
    if let Ok(env_timeout) = env::var(COMMAND_TIMEOUT_ENV_VAR)
        && let Ok(timeout) = env_timeout.parse::<u64>()
    {
        return timeout;
    }

    // Priority 3: Default value
    DEFAULT_COMMAND_TIMEOUT_SECS
}

/// Resolve the max retries value with priority: parameter -> env var -> default
pub(crate) fn resolve_max_retries(max_retries_param: Option<u32>) -> u32 {
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
pub(crate) fn resolve_retry_delay_ms(retry_delay_param: Option<u64>) -> u64 {
    // Priority 1: Use parameter if provided
    if let Some(delay) = retry_delay_param {
        return delay;
    }

    // Priority 2: Use environment variable if set
    if let Ok(env_delay) = env::var(RETRY_DELAY_MS_ENV_VAR)
        && let Ok(delay) = env_delay.parse::<u64>()
    {
        return delay;
    }

    // Priority 3: Default value
    DEFAULT_RETRY_DELAY_MS
}

/// Resolve the compression setting with priority: parameter -> env var -> default (true)
pub(crate) fn resolve_compression(compress_param: Option<bool>) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    // Use a mutex to serialize env var tests to avoid race conditions
    // SAFETY: Tests are serialized via ENV_TEST_MUTEX to prevent data races
    static ENV_TEST_MUTEX: once_cell::sync::Lazy<StdMutex<()>> =
        once_cell::sync::Lazy::new(|| StdMutex::new(()));

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

    mod config_resolution {
        use super::*;

        mod connect_timeout {
            use super::*;

            #[test]
            fn test_uses_param_when_provided() {
                let result = resolve_connect_timeout(Some(60));
                assert_eq!(result, 60);
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
                assert_eq!(result, 45);
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
                assert_eq!(result, 90);
            }

            #[test]
            fn test_uses_default_when_no_param_or_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(CONNECT_TIMEOUT_ENV_VAR);
                }
                let result = resolve_connect_timeout(None);
                assert_eq!(result, DEFAULT_CONNECT_TIMEOUT_SECS);
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
                assert_eq!(result, DEFAULT_CONNECT_TIMEOUT_SECS);
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
                assert_eq!(result, DEFAULT_CONNECT_TIMEOUT_SECS);
            }
        }

        mod command_timeout {
            use super::*;

            #[test]
            fn test_uses_param_when_provided() {
                let result = resolve_command_timeout(Some(120));
                assert_eq!(result, 120);
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
                assert_eq!(result, 60);
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
                assert_eq!(result, 240);
            }

            #[test]
            fn test_uses_default_when_no_param_or_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(COMMAND_TIMEOUT_ENV_VAR);
                }
                let result = resolve_command_timeout(None);
                assert_eq!(result, DEFAULT_COMMAND_TIMEOUT_SECS);
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
                assert_eq!(result, DEFAULT_COMMAND_TIMEOUT_SECS);
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

        mod retry_delay_ms {
            use super::*;

            #[test]
            fn test_uses_param_when_provided() {
                let result = resolve_retry_delay_ms(Some(2000));
                assert_eq!(result, 2000);
            }

            #[test]
            fn test_param_takes_priority_over_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(RETRY_DELAY_MS_ENV_VAR, "5000");
                }
                let result = resolve_retry_delay_ms(Some(500));
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(RETRY_DELAY_MS_ENV_VAR);
                }
                assert_eq!(result, 500);
            }

            #[test]
            fn test_uses_env_var_when_no_param() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(RETRY_DELAY_MS_ENV_VAR, "3000");
                }
                let result = resolve_retry_delay_ms(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(RETRY_DELAY_MS_ENV_VAR);
                }
                assert_eq!(result, 3000);
            }

            #[test]
            fn test_uses_default_when_no_param_or_env() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(RETRY_DELAY_MS_ENV_VAR);
                }
                let result = resolve_retry_delay_ms(None);
                assert_eq!(result, DEFAULT_RETRY_DELAY_MS);
            }

            #[test]
            fn test_ignores_invalid_env_var() {
                let _guard = ENV_TEST_MUTEX.lock().unwrap();
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    set_env(RETRY_DELAY_MS_ENV_VAR, "xyz");
                }
                let result = resolve_retry_delay_ms(None);
                // SAFETY: Holding ENV_TEST_MUTEX, no concurrent env access
                unsafe {
                    remove_env(RETRY_DELAY_MS_ENV_VAR);
                }
                assert_eq!(result, DEFAULT_RETRY_DELAY_MS);
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
    }
}
