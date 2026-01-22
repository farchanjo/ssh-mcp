//! Error classification for SSH connection retry logic.
//!
//! This module provides error classification to determine which errors are transient
//! and worth retrying versus permanent failures that should fail immediately.
//!
//! # Classification Strategy
//!
//! Errors are classified into three categories:
//!
//! 1. **Authentication Failures (NOT retryable)**: These indicate credential or
//!    permission issues that will not resolve by retrying. Examples include wrong
//!    passwords, invalid keys, or denied access.
//!
//! 2. **Connection Errors (retryable)**: These are transient network issues that
//!    may resolve on retry. Examples include connection refused, timeout, or
//!    temporary DNS failures.
//!
//! 3. **Unknown Errors (default behavior)**: Errors that don't match known patterns
//!    are treated conservatively. If they don't contain "ssh" in the message, they
//!    are considered retryable. SSH protocol errors are generally not retried unless
//!    they also indicate a connection/timeout issue.
//!
//! # Priority
//!
//! Authentication errors take precedence over connection errors. If an error message
//! contains both authentication and connection keywords, it is classified as
//! non-retryable to avoid repeatedly failing with bad credentials.
//!
//! # Examples
//!
//! ```rust,ignore
//! use ssh_mcp::mcp::error::is_retryable_error;
//!
//! // Authentication failures are NOT retryable
//! assert!(!is_retryable_error("Permission denied"));
//! assert!(!is_retryable_error("Authentication failed"));
//!
//! // Connection errors ARE retryable
//! assert!(is_retryable_error("Connection refused"));
//! assert!(is_retryable_error("Network is unreachable"));
//!
//! // SSH protocol errors are NOT retryable by default
//! assert!(!is_retryable_error("SSH protocol error"));
//!
//! // But SSH errors with timeout/connect keywords ARE retryable
//! assert!(is_retryable_error("SSH connection timeout"));
//! ```

/// Authentication error patterns that indicate permanent failures.
///
/// These errors will never succeed by retrying and should fail immediately
/// to avoid wasting time and potentially locking out accounts.
const AUTH_ERRORS: &[&str] = &[
    "authentication failed",
    "password authentication failed",
    "key authentication failed",
    "agent authentication failed",
    "permission denied",
    "publickey",
    "auth fail",
    "no authentication",
    "all authentication methods failed",
];

/// Connection error patterns that indicate transient failures.
///
/// These errors may resolve on retry due to temporary network conditions,
/// server load, or other transient issues.
const RETRYABLE_ERRORS: &[&str] = &[
    "connection refused",
    "connection reset",
    "connection timed out",
    "timeout",
    "network is unreachable",
    "no route to host",
    "host is down",
    "temporary failure",
    "resource temporarily unavailable",
    "handshake failed",
    "failed to connect",
    "broken pipe",
    "would block",
];

/// Determines if an error is retryable (transient) or permanent.
///
/// This function analyzes error messages to classify them as either transient
/// connection errors that may succeed on retry, or permanent failures that
/// should not be retried.
///
/// # Arguments
///
/// * `error` - The error message to classify
///
/// # Returns
///
/// * `true` if the error is transient and the operation should be retried
/// * `false` if the error is permanent and should not be retried
///
/// # Classification Rules
///
/// 1. **Authentication failures are NOT retryable**: Errors containing patterns
///    like "authentication failed", "permission denied", or "publickey" indicate
///    credential issues that won't resolve by retrying.
///
/// 2. **Connection errors ARE retryable**: Errors containing patterns like
///    "connection refused", "timeout", or "network is unreachable" are transient
///    and may succeed on retry.
///
/// 3. **Unknown errors default behavior**: For errors that don't match known
///    patterns:
///    - If the error contains "ssh" (case-insensitive), it's NOT retried unless
///      it also contains "timeout" or "connect"
///    - If the error doesn't contain "ssh", it IS retried (conservative approach
///      for potentially transient issues)
///
/// # Priority
///
/// Authentication errors are checked first and take precedence. An error like
/// "Connection timeout during authentication failed" will be classified as
/// NOT retryable because it contains authentication failure keywords.
///
/// # Examples
///
/// ```rust,ignore
/// // Auth errors - NOT retryable
/// assert!(!is_retryable_error("Permission denied"));
/// assert!(!is_retryable_error("Authentication failed"));
///
/// // Connection errors - ARE retryable
/// assert!(is_retryable_error("Connection refused"));
/// assert!(is_retryable_error("timeout"));
///
/// // SSH protocol errors - NOT retryable unless timeout/connect
/// assert!(!is_retryable_error("SSH protocol error"));
/// assert!(is_retryable_error("SSH connection timeout"));
/// ```
pub(crate) fn is_retryable_error(error: &str) -> bool {
    let error_lower = error.to_lowercase();

    // Authentication failures are NOT retryable (checked first for priority)
    for auth_err in AUTH_ERRORS {
        if error_lower.contains(auth_err) {
            return false;
        }
    }

    // Connection errors ARE retryable
    for retryable_err in RETRYABLE_ERRORS {
        if error_lower.contains(retryable_err) {
            return true;
        }
    }

    // Default: retry on unknown errors (conservative approach for transient issues)
    // But if it looks like an SSH protocol error, don't retry
    !error_lower.contains("ssh")
        || error_lower.contains("timeout")
        || error_lower.contains("connect")
}

#[cfg(test)]
mod tests {
    use super::*;

    mod auth_errors_not_retryable {
        use super::*;

        #[test]
        fn test_authentication_failed() {
            assert!(!is_retryable_error("Authentication failed"));
            assert!(!is_retryable_error("AUTHENTICATION FAILED"));
            assert!(!is_retryable_error("authentication failed for user"));
        }

        #[test]
        fn test_password_authentication_failed() {
            assert!(!is_retryable_error("Password authentication failed"));
            assert!(!is_retryable_error(
                "password authentication failed: wrong password"
            ));
        }

        #[test]
        fn test_key_authentication_failed() {
            assert!(!is_retryable_error("Key authentication failed"));
            assert!(!is_retryable_error(
                "key authentication failed: invalid key"
            ));
        }

        #[test]
        fn test_agent_authentication_failed() {
            assert!(!is_retryable_error("Agent authentication failed"));
            assert!(!is_retryable_error("agent authentication failed: no keys"));
        }

        #[test]
        fn test_permission_denied() {
            assert!(!is_retryable_error("Permission denied"));
            assert!(!is_retryable_error("permission denied (publickey)"));
            assert!(!is_retryable_error("PERMISSION DENIED"));
        }

        #[test]
        fn test_publickey_error() {
            assert!(!is_retryable_error("publickey"));
            assert!(!is_retryable_error("Publickey authentication required"));
        }

        #[test]
        fn test_auth_fail() {
            assert!(!is_retryable_error("auth fail"));
            assert!(!is_retryable_error("Auth fail: invalid credentials"));
        }

        #[test]
        fn test_no_authentication() {
            assert!(!is_retryable_error("No authentication methods available"));
            assert!(!is_retryable_error("no authentication methods succeeded"));
        }

        #[test]
        fn test_all_auth_methods_failed() {
            assert!(!is_retryable_error("All authentication methods failed"));
        }
    }

    mod connection_errors_retryable {
        use super::*;

        #[test]
        fn test_connection_refused() {
            assert!(is_retryable_error("Connection refused"));
            assert!(is_retryable_error("connection refused by server"));
        }

        #[test]
        fn test_connection_reset() {
            assert!(is_retryable_error("Connection reset"));
            assert!(is_retryable_error("connection reset by peer"));
        }

        #[test]
        fn test_connection_timed_out() {
            assert!(is_retryable_error("Connection timed out"));
            assert!(is_retryable_error("connection timed out after 30s"));
        }

        #[test]
        fn test_timeout() {
            assert!(is_retryable_error("timeout"));
            assert!(is_retryable_error("Operation timeout"));
            assert!(is_retryable_error("TIMEOUT waiting for response"));
        }

        #[test]
        fn test_network_unreachable() {
            assert!(is_retryable_error("Network is unreachable"));
            assert!(is_retryable_error("network is unreachable"));
        }

        #[test]
        fn test_no_route_to_host() {
            assert!(is_retryable_error("No route to host"));
            assert!(is_retryable_error("no route to host"));
        }

        #[test]
        fn test_host_is_down() {
            assert!(is_retryable_error("Host is down"));
            assert!(is_retryable_error("host is down"));
        }

        #[test]
        fn test_temporary_failure() {
            assert!(is_retryable_error("Temporary failure in name resolution"));
            assert!(is_retryable_error("temporary failure"));
        }

        #[test]
        fn test_resource_temporarily_unavailable() {
            assert!(is_retryable_error("Resource temporarily unavailable"));
        }

        #[test]
        fn test_handshake_failed() {
            assert!(is_retryable_error("Handshake failed"));
            assert!(is_retryable_error("SSH handshake failed"));
        }

        #[test]
        fn test_failed_to_connect() {
            assert!(is_retryable_error("Failed to connect"));
            assert!(is_retryable_error("failed to connect to server"));
        }

        #[test]
        fn test_broken_pipe() {
            assert!(is_retryable_error("Broken pipe"));
            assert!(is_retryable_error("broken pipe error"));
        }

        #[test]
        fn test_would_block() {
            assert!(is_retryable_error("Would block"));
            assert!(is_retryable_error("operation would block"));
        }
    }

    mod edge_cases {
        use super::*;

        #[test]
        fn test_empty_string_is_retryable() {
            // Empty string doesn't match any auth patterns, doesn't contain "ssh"
            assert!(is_retryable_error(""));
        }

        #[test]
        fn test_unknown_error_without_ssh() {
            // Unknown errors that don't contain "ssh" are retryable (conservative)
            assert!(is_retryable_error("Something went wrong"));
        }

        #[test]
        fn test_ssh_protocol_error_not_retryable() {
            // SSH protocol errors without timeout/connect keywords are not retryable
            assert!(!is_retryable_error("SSH protocol error"));
            assert!(!is_retryable_error("SSH version mismatch"));
        }

        #[test]
        fn test_ssh_with_timeout_is_retryable() {
            // SSH errors with timeout keyword are retryable
            assert!(is_retryable_error("SSH connection timeout"));
        }

        #[test]
        fn test_ssh_with_connect_is_retryable() {
            // SSH errors with connect keyword are retryable
            assert!(is_retryable_error("SSH failed to connect"));
        }

        #[test]
        fn test_case_insensitivity() {
            assert!(!is_retryable_error("PERMISSION DENIED"));
            assert!(is_retryable_error("CONNECTION REFUSED"));
        }

        #[test]
        fn test_auth_error_takes_precedence_over_connection() {
            // If both auth and connection keywords present, auth should win
            assert!(!is_retryable_error(
                "Connection timeout during authentication failed"
            ));
        }
    }
}
