//! Serializable response types for MCP SSH tools.
//!
//! This module contains all request and response types used by the MCP SSH commands.
//! All types implement `Serialize`, `Deserialize`, and `JsonSchema` for proper
//! MCP protocol compatibility.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Session metadata for tracking connection information
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionInfo {
    pub session_id: String,
    /// Optional human-readable name for the session (useful for LLM identification)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub host: String,
    pub username: String,
    pub connected_at: String,
    /// Default timeout in seconds used for this session's connection
    pub default_timeout_secs: u64,
    /// Number of retry attempts needed to establish the connection
    pub retry_attempts: u32,
    /// Whether compression is enabled for this session
    pub compression_enabled: bool,
    /// Timestamp of last health check (RFC3339 format)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_health_check: Option<String>,
    /// Whether session passed last health check
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthy: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SshConnectResponse {
    pub session_id: String,
    pub message: String,
    pub authenticated: bool,
    /// Number of retry attempts needed to establish the connection
    pub retry_attempts: u32,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SshCommandResponse {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    /// Whether the command timed out (partial output may be available)
    #[serde(default)]
    pub timed_out: bool,
}

/// Port forwarding response (only functional when port_forward feature is enabled)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PortForwardingResponse {
    pub local_address: String,
    pub remote_address: String,
    pub active: bool,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SessionListResponse {
    /// List of active SSH sessions
    pub sessions: Vec<SessionInfo>,
    /// Total number of active sessions
    pub count: usize,
}

#[cfg(test)]
mod response_serialization {
    use super::*;

    mod ssh_connect_response {
        use super::*;

        #[test]
        fn test_serialize_and_deserialize() {
            let response = SshConnectResponse {
                session_id: "test-uuid-123".to_string(),
                message: "Connected successfully".to_string(),
                authenticated: true,
                retry_attempts: 2,
            };

            let json = serde_json::to_string(&response).unwrap();
            let deserialized: SshConnectResponse = serde_json::from_str(&json).unwrap();

            assert_eq!(deserialized.session_id, "test-uuid-123");
            assert_eq!(deserialized.message, "Connected successfully");
            assert!(deserialized.authenticated);
            assert_eq!(deserialized.retry_attempts, 2);
        }

        #[test]
        fn test_json_structure() {
            let response = SshConnectResponse {
                session_id: "abc".to_string(),
                message: "msg".to_string(),
                authenticated: false,
                retry_attempts: 0,
            };

            let json = serde_json::to_value(&response).unwrap();

            assert!(json.get("session_id").is_some());
            assert!(json.get("message").is_some());
            assert!(json.get("authenticated").is_some());
            assert!(json.get("retry_attempts").is_some());
        }
    }

    mod ssh_command_response {
        use super::*;

        #[test]
        fn test_serialize_and_deserialize() {
            let response = SshCommandResponse {
                stdout: "Hello, World!".to_string(),
                stderr: "Warning: something".to_string(),
                exit_code: 0,
                timed_out: false,
            };

            let json = serde_json::to_string(&response).unwrap();
            let deserialized: SshCommandResponse = serde_json::from_str(&json).unwrap();

            assert_eq!(deserialized.stdout, "Hello, World!");
            assert_eq!(deserialized.stderr, "Warning: something");
            assert_eq!(deserialized.exit_code, 0);
            assert!(!deserialized.timed_out);
        }

        #[test]
        fn test_negative_exit_code() {
            let response = SshCommandResponse {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: -1,
                timed_out: false,
            };

            let json = serde_json::to_string(&response).unwrap();
            let deserialized: SshCommandResponse = serde_json::from_str(&json).unwrap();

            assert_eq!(deserialized.exit_code, -1);
        }

        #[test]
        fn test_empty_output() {
            let response = SshCommandResponse {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 127,
                timed_out: false,
            };

            let json = serde_json::to_string(&response).unwrap();
            let deserialized: SshCommandResponse = serde_json::from_str(&json).unwrap();

            assert_eq!(deserialized.stdout, "");
            assert_eq!(deserialized.stderr, "");
            assert_eq!(deserialized.exit_code, 127);
        }

        #[test]
        fn test_unicode_content() {
            let response = SshCommandResponse {
                stdout: "Hello, \u{4e16}\u{754c}!".to_string(), // Hello, World! in Chinese
                stderr: String::new(),
                exit_code: 0,
                timed_out: false,
            };

            let json = serde_json::to_string(&response).unwrap();
            let deserialized: SshCommandResponse = serde_json::from_str(&json).unwrap();

            assert!(deserialized.stdout.contains('\u{4e16}'));
        }

        #[test]
        fn test_timed_out_response() {
            let response = SshCommandResponse {
                stdout: "partial output".to_string(),
                stderr: String::new(),
                exit_code: -1,
                timed_out: true,
            };

            let json = serde_json::to_string(&response).unwrap();
            let deserialized: SshCommandResponse = serde_json::from_str(&json).unwrap();

            assert!(deserialized.timed_out);
            assert_eq!(deserialized.stdout, "partial output");
            assert_eq!(deserialized.exit_code, -1);
        }

        #[test]
        fn test_timed_out_defaults_to_false() {
            // Test that timed_out defaults to false when not present in JSON
            let json = r#"{"stdout":"test","stderr":"","exit_code":0}"#;
            let deserialized: SshCommandResponse = serde_json::from_str(json).unwrap();

            assert!(!deserialized.timed_out);
        }
    }

    mod session_info {
        use super::*;

        #[test]
        fn test_serialize_and_deserialize() {
            let info = SessionInfo {
                session_id: "uuid-123".to_string(),
                name: Some("production-db".to_string()),
                host: "192.168.1.1:22".to_string(),
                username: "testuser".to_string(),
                connected_at: "2024-01-15T10:30:00Z".to_string(),
                default_timeout_secs: 30,
                retry_attempts: 1,
                compression_enabled: true,
                last_health_check: Some("2024-01-15T10:35:00Z".to_string()),
                healthy: Some(true),
            };

            let json = serde_json::to_string(&info).unwrap();
            let deserialized: SessionInfo = serde_json::from_str(&json).unwrap();

            assert_eq!(deserialized.session_id, "uuid-123");
            assert_eq!(deserialized.name, Some("production-db".to_string()));
            assert_eq!(deserialized.host, "192.168.1.1:22");
            assert_eq!(deserialized.username, "testuser");
            assert_eq!(deserialized.connected_at, "2024-01-15T10:30:00Z");
            assert_eq!(deserialized.default_timeout_secs, 30);
            assert_eq!(deserialized.retry_attempts, 1);
            assert!(deserialized.compression_enabled);
            assert_eq!(
                deserialized.last_health_check,
                Some("2024-01-15T10:35:00Z".to_string())
            );
            assert_eq!(deserialized.healthy, Some(true));
        }

        #[test]
        fn test_serialize_without_name() {
            let info = SessionInfo {
                session_id: "uuid-456".to_string(),
                name: None,
                host: "192.168.1.1:22".to_string(),
                username: "testuser".to_string(),
                connected_at: "2024-01-15T10:30:00Z".to_string(),
                default_timeout_secs: 30,
                retry_attempts: 0,
                compression_enabled: true,
                last_health_check: None,
                healthy: None,
            };

            let json = serde_json::to_string(&info).unwrap();
            // When name is None, it should not appear in JSON due to skip_serializing_if
            // Check for "name": pattern to avoid matching "username"
            assert!(!json.contains("\"name\":"));
            // Health check fields should also be omitted when None
            assert!(!json.contains("\"last_health_check\":"));
            assert!(!json.contains("\"healthy\":"));

            let deserialized: SessionInfo = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized.name, None);
            assert_eq!(deserialized.last_health_check, None);
            assert_eq!(deserialized.healthy, None);
        }

        #[test]
        fn test_clone() {
            let info = SessionInfo {
                session_id: "clone-test".to_string(),
                name: Some("test-session".to_string()),
                host: "host".to_string(),
                username: "user".to_string(),
                connected_at: "now".to_string(),
                default_timeout_secs: 60,
                retry_attempts: 0,
                compression_enabled: false,
                last_health_check: Some("2024-01-15T10:30:00Z".to_string()),
                healthy: Some(true),
            };

            let cloned = info.clone();

            assert_eq!(cloned.session_id, info.session_id);
            assert_eq!(cloned.name, info.name);
            assert_eq!(cloned.compression_enabled, info.compression_enabled);
            assert_eq!(cloned.last_health_check, info.last_health_check);
            assert_eq!(cloned.healthy, info.healthy);
        }
    }

    mod session_list_response {
        use super::*;

        #[test]
        fn test_empty_list() {
            let response = SessionListResponse {
                sessions: vec![],
                count: 0,
            };

            let json = serde_json::to_string(&response).unwrap();
            let deserialized: SessionListResponse = serde_json::from_str(&json).unwrap();

            assert!(deserialized.sessions.is_empty());
            assert_eq!(deserialized.count, 0);
        }

        #[test]
        fn test_multiple_sessions() {
            let session1 = SessionInfo {
                session_id: "s1".to_string(),
                name: Some("production".to_string()),
                host: "host1".to_string(),
                username: "user1".to_string(),
                connected_at: "t1".to_string(),
                default_timeout_secs: 30,
                retry_attempts: 0,
                compression_enabled: true,
                last_health_check: Some("2024-01-15T10:30:00Z".to_string()),
                healthy: Some(true),
            };
            let session2 = SessionInfo {
                session_id: "s2".to_string(),
                name: None,
                host: "host2".to_string(),
                username: "user2".to_string(),
                connected_at: "t2".to_string(),
                default_timeout_secs: 60,
                retry_attempts: 2,
                compression_enabled: false,
                last_health_check: None,
                healthy: None,
            };

            let response = SessionListResponse {
                sessions: vec![session1, session2],
                count: 2,
            };

            let json = serde_json::to_string(&response).unwrap();
            let deserialized: SessionListResponse = serde_json::from_str(&json).unwrap();

            assert_eq!(deserialized.sessions.len(), 2);
            assert_eq!(deserialized.count, 2);
            assert_eq!(deserialized.sessions[0].session_id, "s1");
            assert_eq!(
                deserialized.sessions[0].name,
                Some("production".to_string())
            );
            assert_eq!(deserialized.sessions[0].healthy, Some(true));
            assert_eq!(deserialized.sessions[1].session_id, "s2");
            assert_eq!(deserialized.sessions[1].name, None);
            assert_eq!(deserialized.sessions[1].healthy, None);
        }
    }

    #[cfg(feature = "port_forward")]
    mod port_forwarding_response {
        use super::*;

        #[test]
        fn test_serialize_and_deserialize() {
            let response = PortForwardingResponse {
                local_address: "127.0.0.1:8080".to_string(),
                remote_address: "localhost:3306".to_string(),
                active: true,
            };

            let json = serde_json::to_string(&response).unwrap();
            let deserialized: PortForwardingResponse = serde_json::from_str(&json).unwrap();

            assert_eq!(deserialized.local_address, "127.0.0.1:8080");
            assert_eq!(deserialized.remote_address, "localhost:3306");
            assert!(deserialized.active);
        }
    }
}
