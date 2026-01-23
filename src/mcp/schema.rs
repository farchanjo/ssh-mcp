//! JSON Schema helpers for MCP-compliant schemas.
//!
//! Generates standard JSON Schema without Rust-specific formats like "uint"
//! that LLMs may not understand correctly.

use schemars::Schema;
use schemars::json_schema;

/// Unsigned integer schema: `{"type": "integer", "minimum": 0}`
///
/// Use with `#[schemars(schema_with = "crate::mcp::schema::uint")]` on unsigned fields.
pub fn uint(_generator: &mut schemars::SchemaGenerator) -> Schema {
    json_schema!({
        "type": "integer",
        "minimum": 0
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use schemars::SchemaGenerator;

    #[test]
    fn test_uint_schema_structure() {
        let mut generator = SchemaGenerator::default();
        let schema = uint(&mut generator);

        let json = serde_json::to_value(&schema).expect("Failed to serialize schema");

        assert_eq!(json.get("type"), Some(&serde_json::json!("integer")));
        assert_eq!(json.get("minimum"), Some(&serde_json::json!(0)));
        assert!(json.get("format").is_none(), "Should not have format field");
    }

    #[test]
    fn test_uint_schema_no_uint_format() {
        let mut generator = SchemaGenerator::default();
        let schema = uint(&mut generator);

        let json_str = serde_json::to_string(&schema).expect("Failed to serialize schema");

        assert!(
            !json_str.contains("uint"),
            "Schema should not contain 'uint' format"
        );
    }

    #[test]
    fn test_session_list_response_schema_no_uint() {
        use crate::mcp::types::SessionListResponse;

        let schema = SchemaGenerator::default().into_root_schema_for::<SessionListResponse>();
        let json_str = serde_json::to_string(&schema).expect("Failed to serialize schema");

        assert!(
            !json_str.contains("\"uint"),
            "SessionListResponse schema should not contain 'uint' format: {}",
            json_str
        );
    }

    #[test]
    fn test_agent_disconnect_response_schema_no_uint() {
        use crate::mcp::types::AgentDisconnectResponse;

        let schema = SchemaGenerator::default().into_root_schema_for::<AgentDisconnectResponse>();
        let json_str = serde_json::to_string(&schema).expect("Failed to serialize schema");

        assert!(
            !json_str.contains("\"uint"),
            "AgentDisconnectResponse schema should not contain 'uint' format: {}",
            json_str
        );
    }

    #[test]
    fn test_session_info_schema_no_uint() {
        use crate::mcp::types::SessionInfo;

        let schema = SchemaGenerator::default().into_root_schema_for::<SessionInfo>();
        let json_str = serde_json::to_string(&schema).expect("Failed to serialize schema");

        assert!(
            !json_str.contains("\"uint"),
            "SessionInfo schema should not contain 'uint' format: {}",
            json_str
        );
    }

    #[test]
    fn test_ssh_connect_response_schema_no_uint() {
        use crate::mcp::types::SshConnectResponse;

        let schema = SchemaGenerator::default().into_root_schema_for::<SshConnectResponse>();
        let json_str = serde_json::to_string(&schema).expect("Failed to serialize schema");

        assert!(
            !json_str.contains("\"uint"),
            "SshConnectResponse schema should not contain 'uint' format: {}",
            json_str
        );
    }

    #[test]
    fn test_ssh_list_commands_response_schema_no_uint() {
        use crate::mcp::types::SshListCommandsResponse;

        let schema = SchemaGenerator::default().into_root_schema_for::<SshListCommandsResponse>();
        let json_str = serde_json::to_string(&schema).expect("Failed to serialize schema");

        assert!(
            !json_str.contains("\"uint"),
            "SshListCommandsResponse schema should not contain 'uint' format: {}",
            json_str
        );
    }
}
